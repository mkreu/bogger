use std::collections::HashMap;
use std::fs;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use fs2::FileExt;
use serde::Deserialize;

use crate::state::{LastRunResult, StateStore};

pub const DEFAULT_CONFIG_PATH: &str = "/etc/borg-reminder/config.toml";

const DEFAULT_STATE_PATH: &str = "/var/lib/borg-reminder/state.json";
const DEFAULT_LOCK_PATH: &str = "/run/borg-reminder/lock";

#[derive(Debug, Clone, Deserialize)]
pub struct BackupConfig {
    #[serde(default)]
    general: GeneralConfig,
    #[serde(default, rename = "profile")]
    profiles: Vec<ProfileConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct GeneralConfig {
    state_path: Option<PathBuf>,
    lock_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProfileConfig {
    name: String,
    snapper_config: String,
    repo: String,
    passphrase: String,
    archive_prefix: String,
    #[serde(default)]
    extra_args: Vec<String>,
}

#[derive(Debug, Clone)]
struct SnapshotRecord {
    snapshot_number: u64,
    snapshot_path: PathBuf,
}

impl BackupConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read config {}", path.display()))?;
        let config: Self = toml::from_str(&contents)
            .with_context(|| format!("failed to parse config {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    fn state_path(&self) -> &Path {
        self.general
            .state_path
            .as_deref()
            .unwrap_or_else(|| Path::new(DEFAULT_STATE_PATH))
    }

    fn lock_path(&self) -> &Path {
        self.general
            .lock_path
            .as_deref()
            .unwrap_or_else(|| Path::new(DEFAULT_LOCK_PATH))
    }

    fn validate(&self) -> Result<()> {
        if self
            .general
            .state_path
            .as_ref()
            .is_some_and(|path| path.as_os_str().is_empty())
        {
            bail!("general.state_path must not be empty");
        }

        if self
            .general
            .lock_path
            .as_ref()
            .is_some_and(|path| path.as_os_str().is_empty())
        {
            bail!("general.lock_path must not be empty");
        }

        for profile in &self.profiles {
            if profile.name.trim().is_empty() {
                bail!("profile.name must not be empty");
            }
            if profile.snapper_config.trim().is_empty() {
                bail!("profile.snapper_config must not be empty");
            }
            if profile.repo.trim().is_empty() {
                bail!("profile.repo must not be empty");
            }
            if profile.passphrase.is_empty() {
                bail!("profile.passphrase must not be empty");
            }
            if profile.archive_prefix.trim().is_empty() {
                bail!("profile.archive_prefix must not be empty");
            }
        }

        Ok(())
    }
}

pub fn run_backup(config: &BackupConfig) -> Result<()> {
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
    store.save(&state)?;

    eprintln!("starting borg-reminder backup job pid={job_id}");

    let result = run_backup_inner(config, started_at);

    match result {
        Ok(archives_created) => {
            state.last_success_at = Some(Utc::now());
            state.started_at = None;
            state.last_run_result = Some(LastRunResult::Success);
            store.save(&state)?;

            eprintln!(
                "borg-reminder backup completed successfully archives={}",
                archives_created
            );
        }
        Err(error) => {
            state.started_at = None;
            state.last_run_result = Some(LastRunResult::Error);
            store.save(&state)?;
            eprintln!("borg-reminder backup failed: {error:#}");
            drop(lock_file);
            return Err(error);
        }
    }

    drop(lock_file);
    Ok(())
}

fn run_backup_inner(config: &BackupConfig, started_at: chrono::DateTime<Utc>) -> Result<usize> {
    let mut snapshot_records = Vec::with_capacity(config.profiles.len());

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
        eprintln!(
            "created borg archive profile={} repo={} archive={}",
            profile.name, profile.repo, archive
        );
    }

    Ok(snapshot_records.len())
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{BackupConfig, DEFAULT_LOCK_PATH, DEFAULT_STATE_PATH};

    #[test]
    fn parses_minimal_backend_config_with_defaults() {
        let config: BackupConfig = toml::from_str(
            r#"
                [[profile]]
                name = "root"
                snapper_config = "root"
                repo = "repo"
                passphrase = "secret"
                archive_prefix = "host-root"
            "#,
        )
        .unwrap();

        config.validate().unwrap();

        assert_eq!(config.state_path(), Path::new(DEFAULT_STATE_PATH));
        assert_eq!(config.lock_path(), Path::new(DEFAULT_LOCK_PATH));
        assert_eq!(config.profiles.len(), 1);
    }

    #[test]
    fn ignores_frontend_only_config_sections() {
        let config: BackupConfig = toml::from_str(
            r#"
                [general]
                state_path = "/tmp/state.json"
                service_name = "custom.service"

                [status]
                warn_after_hours = 12
                stale_after_hours = 24
                poll_seconds = 15

                [[profile]]
                name = "root"
                snapper_config = "root"
                repo = "repo"
                passphrase = "secret"
                archive_prefix = "host-root"
            "#,
        )
        .unwrap();

        config.validate().unwrap();
        assert_eq!(config.state_path(), Path::new("/tmp/state.json"));
    }
}
