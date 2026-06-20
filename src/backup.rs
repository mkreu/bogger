use std::collections::HashMap;
use std::fs;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use fs2::FileExt;

use crate::config::{Config, ProfileConfig};
use crate::state::{ArchiveRecord, LastRunResult, SnapshotRecord, StateStore};

pub fn run_backup(config: &Config) -> Result<()> {
    if config.profiles.is_empty() {
        bail!("at least one [[profile]] must be configured for backup");
    }

    let lock_file = acquire_lock(config.lock_path())?;
    let store = StateStore::new(config.state_path().to_path_buf());
    let mut state = store.load()?;
    let started_at = Utc::now();
    let job_id = std::process::id();

    state.last_attempt_at = Some(started_at);
    state.started_at = Some(started_at);
    state.active_job_id = Some(job_id);
    state.last_error = None;
    state.archives_created.clear();
    state.snapshots_used.clear();
    store.save(&state)?;

    eprintln!("starting borg-reminder backup job pid={job_id}");

    let result = run_backup_inner(config, started_at);

    match result {
        Ok((snapshot_records, archive_records)) => {
            state.last_success_at = Some(Utc::now());
            state.started_at = None;
            state.active_job_id = None;
            state.last_error = None;
            state.last_run_result = Some(LastRunResult::Success);
            state.snapshots_used = snapshot_records;
            state.archives_created = archive_records;
            store.save(&state)?;

            eprintln!(
                "borg-reminder backup completed successfully archives={}",
                state.archives_created.len()
            );
        }
        Err(error) => {
            state.started_at = None;
            state.active_job_id = None;
            state.last_error = Some(format!("{error:#}"));
            state.last_run_result = Some(LastRunResult::Error);
            store.save(&state)?;
            drop(lock_file);
            return Err(error);
        }
    }

    drop(lock_file);
    Ok(())
}

fn run_backup_inner(
    config: &Config,
    started_at: chrono::DateTime<Utc>,
) -> Result<(Vec<SnapshotRecord>, Vec<ArchiveRecord>)> {
    let mut snapshot_records = Vec::with_capacity(config.profiles.len());
    let mut archive_records = Vec::with_capacity(config.profiles.len());

    for profile in &config.profiles {
        let snapshot = discover_latest_snapshot(profile)?;
        eprintln!(
            "selected snapshot profile={} number={} path={}",
            profile.name,
            snapshot.snapshot_number,
            snapshot.snapshot_path.display()
        );
        let archive = create_archive_name(&profile.archive_prefix, started_at);
        run_borg_create(profile, &archive, &snapshot.snapshot_path)?;

        snapshot_records.push(snapshot);
        archive_records.push(ArchiveRecord {
            profile: profile.name.clone(),
            archive,
            repo: profile.repo.clone(),
        });
    }

    Ok((snapshot_records, archive_records))
}

fn acquire_lock(lock_path: &Path) -> Result<File> {
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create lock directory {}", parent.display()))?;
    }

    let file = File::create(lock_path)
        .with_context(|| format!("failed to open lock file {}", lock_path.display()))?;

    file.try_lock_exclusive()
        .with_context(|| format!("backup already running: {}", lock_path.display()))?;

    Ok(file)
}

fn discover_latest_snapshot(profile: &ProfileConfig) -> Result<SnapshotRecord> {
    let snapper_config = read_snapper_config(&profile.snapper_config)?;
    let subvolume = snapper_config.get("SUBVOLUME").cloned().ok_or_else(|| {
        anyhow::anyhow!(
            "Snapper config {} is missing SUBVOLUME",
            profile.snapper_config
        )
    })?;

    let subvolume_path = PathBuf::from(subvolume);
    let snapshots_path = subvolume_path.join(".snapshots");
    let entries = fs::read_dir(&snapshots_path).with_context(|| {
        format!(
            "failed to read snapshots directory {} for profile {}",
            snapshots_path.display(),
            profile.name
        )
    })?;

    let mut latest = None;

    for entry in entries {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        let Ok(number) = file_name.parse::<u64>() else {
            continue;
        };
        let snapshot_path = entry.path().join("snapshot");
        if !snapshot_path.exists() {
            continue;
        }

        match latest {
            Some((current, _)) if current >= number => {}
            _ => latest = Some((number, snapshot_path)),
        }
    }

    let Some((snapshot_number, snapshot_path)) = latest else {
        bail!(
            "no snapshots found in {} for Snapper config {}",
            snapshots_path.display(),
            profile.snapper_config
        );
    };

    Ok(SnapshotRecord {
        profile: profile.name.clone(),
        snapper_config: profile.snapper_config.clone(),
        snapshot_number,
        snapshot_path,
    })
}

fn read_snapper_config(name: &str) -> Result<HashMap<String, String>> {
    let path = Path::new("/etc/snapper/configs").join(name);
    let contents = fs::read_to_string(&path)
        .with_context(|| format!("failed to read Snapper config {}", path.display()))?;
    let mut values = HashMap::new();

    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').to_string();
        values.insert(key.trim().to_string(), value);
    }

    Ok(values)
}

fn run_borg_create(profile: &ProfileConfig, archive: &str, snapshot_path: &Path) -> Result<()> {
    let archive_target = format!("{}::{archive}", profile.repo);
    let mut command = Command::new("borg");
    command.arg("create");
    command.args(&profile.extra_args);
    command.arg(&archive_target);
    command.arg(snapshot_path);
    command.env("BORG_PASSPHRASE", &profile.passphrase);

    eprintln!(
        "running borg create profile={} repo={} archive={} path={}",
        profile.name,
        profile.repo,
        archive,
        snapshot_path.display()
    );

    let output = command
        .output()
        .with_context(|| format!("failed to invoke borg for profile {}", profile.name))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!("borg create failed for profile {}: {stderr}", profile.name);
}

fn create_archive_name(prefix: &str, timestamp: chrono::DateTime<Utc>) -> String {
    format!("{}-{}", prefix, timestamp.format("%Y-%m-%dT%H-%M-%SZ"))
}
