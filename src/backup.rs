use std::collections::{HashMap, HashSet};
use std::fs;
use std::fs::File;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use fs2::FileExt;
use serde::Deserialize;

use crate::state::{LastRunResult, StateStore};

pub const DEFAULT_CONFIG_PATH: &str = "/etc/bogger/config.toml";

const DEFAULT_STATE_PATH: &str = "/var/lib/bogger/state.json";
const DEFAULT_LOCK_PATH: &str = "/run/bogger/lock";
const DEFAULT_MOUNT_DIR: &str = "/run/bogger/mnt";

/// Upper limit for removing stacked leftover mounts from one mount point.
const MAX_LEFTOVER_UNMOUNTS: usize = 8;

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
    mount_dir: Option<PathBuf>,
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
    #[serde(default)]
    exclude: Vec<String>,
    prune: Option<PruneConfig>,
}

/// Retention rules for `borg prune`. Unknown keys are rejected, because a
/// misspelled rule would silently make prune delete more archives.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PruneConfig {
    keep_within: Option<String>,
    keep_last: Option<i32>,
    keep_minutely: Option<i32>,
    keep_hourly: Option<i32>,
    keep_daily: Option<i32>,
    keep_weekly: Option<i32>,
    keep_monthly: Option<i32>,
    keep_13weekly: Option<i32>,
    keep_3monthly: Option<i32>,
    keep_yearly: Option<i32>,
}

impl PruneConfig {
    /// Count rules as (config key, borg flag, value).
    fn count_rules(&self) -> [(&'static str, &'static str, Option<i32>); 9] {
        [
            ("keep_last", "--keep-last", self.keep_last),
            ("keep_minutely", "--keep-minutely", self.keep_minutely),
            ("keep_hourly", "--keep-hourly", self.keep_hourly),
            ("keep_daily", "--keep-daily", self.keep_daily),
            ("keep_weekly", "--keep-weekly", self.keep_weekly),
            ("keep_monthly", "--keep-monthly", self.keep_monthly),
            ("keep_13weekly", "--keep-13weekly", self.keep_13weekly),
            ("keep_3monthly", "--keep-3monthly", self.keep_3monthly),
            ("keep_yearly", "--keep-yearly", self.keep_yearly),
        ]
    }

    fn validate(&self, profile_name: &str) -> Result<()> {
        if self
            .keep_within
            .as_ref()
            .is_some_and(|value| value.trim().is_empty())
        {
            bail!("profile {profile_name}: prune.keep_within must not be empty");
        }

        for (key, _, value) in self.count_rules() {
            // borg reads a negative count as "no limit"; only -1 is accepted.
            if value.is_some_and(|value| value == 0 || value < -1) {
                bail!("profile {profile_name}: prune.{key} must be at least 1, or -1 for no limit");
            }
        }

        let has_rule = self.keep_within.is_some()
            || self
                .count_rules()
                .iter()
                .any(|(_, _, value)| value.is_some());
        if !has_rule {
            bail!("profile {profile_name}: [profile.prune] must set at least one keep_* rule");
        }

        Ok(())
    }

    fn borg_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(within) = &self.keep_within {
            args.push("--keep-within".to_string());
            args.push(within.clone());
        }
        for (_, flag, value) in self.count_rules() {
            if let Some(value) = value {
                args.push(flag.to_string());
                args.push(value.to_string());
            }
        }
        args
    }
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

    fn mount_dir(&self) -> &Path {
        self.general
            .mount_dir
            .as_deref()
            .unwrap_or_else(|| Path::new(DEFAULT_MOUNT_DIR))
    }

    fn validate(&self) -> Result<()> {
        for (key, value) in [
            ("state_path", &self.general.state_path),
            ("lock_path", &self.general.lock_path),
            ("mount_dir", &self.general.mount_dir),
        ] {
            if value
                .as_ref()
                .is_some_and(|path| path.as_os_str().is_empty())
            {
                bail!("general.{key} must not be empty");
            }
        }

        let mut names = HashSet::new();

        for profile in &self.profiles {
            if profile.name.trim().is_empty() {
                bail!("profile.name must not be empty");
            }
            if profile.name.contains('/') || profile.name == "." || profile.name == ".." {
                bail!(
                    "profile.name {:?} must be usable as a directory name (no '/', not '.' or '..')",
                    profile.name
                );
            }
            if !names.insert(profile.name.as_str()) {
                bail!("profile.name {:?} is used more than once", profile.name);
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
            if profile
                .archive_prefix
                .contains(['*', '?', '[', ']', '{', '}'])
            {
                bail!(
                    "profile {}: archive_prefix must not contain glob characters",
                    profile.name
                );
            }
            if profile
                .exclude
                .iter()
                .any(|pattern| pattern.trim().is_empty())
            {
                bail!(
                    "profile {}: exclude entries must not be empty",
                    profile.name
                );
            }
            if let Some(prune) = &profile.prune {
                prune.validate(&profile.name)?;
            }
        }

        self.validate_prefixes()
    }

    /// Prune selects archives by `<prefix>-*`. Reject prefixes on the same
    /// repo where that glob would also match archives of another profile.
    fn validate_prefixes(&self) -> Result<()> {
        for (index, first) in self.profiles.iter().enumerate() {
            for second in &self.profiles[index + 1..] {
                if first.repo != second.repo {
                    continue;
                }
                let first_prefix = format!("{}-", first.archive_prefix);
                let second_prefix = format!("{}-", second.archive_prefix);
                if first_prefix == second_prefix
                    || second.archive_prefix.starts_with(&first_prefix)
                    || first.archive_prefix.starts_with(&second_prefix)
                {
                    bail!(
                        "profiles {} and {} use repo {} with overlapping archive_prefix values {:?} and {:?}",
                        first.name,
                        second.name,
                        first.repo,
                        first.archive_prefix,
                        second.archive_prefix
                    );
                }
            }
        }
        Ok(())
    }
}

struct RunReport {
    archives_created: usize,
    maintenance_errors: usize,
}

impl RunReport {
    /// Log a failed step that runs after the archive exists. The run goes on.
    fn maintenance_failed(&mut self, step: &str, profile: &ProfileConfig, error: anyhow::Error) {
        eprintln!("{step} failed for profile {}: {error:#}", profile.name);
        self.maintenance_errors += 1;
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

    eprintln!("starting bogger backup job pid={job_id}");

    let result = run_backup_inner(config, started_at);
    state.started_at = None;

    let outcome = match result {
        Ok(report) => {
            // All archives exist, so the data is backed up even when
            // unmount, prune or compact failed afterwards.
            state.last_success_at = Some(Utc::now());
            if report.maintenance_errors == 0 {
                state.last_run_result = Some(LastRunResult::Success);
                store.save(&state)?;
                eprintln!(
                    "bogger backup completed successfully archives={}",
                    report.archives_created
                );
                Ok(())
            } else {
                state.last_run_result = Some(LastRunResult::Error);
                store.save(&state)?;
                eprintln!(
                    "bogger backup completed with errors archives={} maintenance_errors={}",
                    report.archives_created, report.maintenance_errors
                );
                Err(anyhow::anyhow!(
                    "{} maintenance step(s) failed, see log above",
                    report.maintenance_errors
                ))
            }
        }
        Err(error) => {
            state.last_run_result = Some(LastRunResult::Error);
            store.save(&state)?;
            eprintln!("bogger backup failed: {error:#}");
            Err(error)
        }
    };

    drop(lock_file);
    outcome
}

fn run_backup_inner(config: &BackupConfig, started_at: chrono::DateTime<Utc>) -> Result<RunReport> {
    let mount_dir = prepare_mount_dir(config.mount_dir())?;
    let mut report = RunReport {
        archives_created: 0,
        maintenance_errors: 0,
    };

    for profile in &config.profiles {
        let snapshot = discover_latest_snapshot(profile)?;
        eprintln!(
            "selected snapshot profile={} number={} path={}",
            profile.name,
            snapshot.snapshot_number,
            snapshot.snapshot_path.display()
        );

        let archive = create_archive_name(&profile.archive_prefix, started_at);
        let mount = SnapshotMount::mount(&snapshot.snapshot_path, &mount_dir.join(&profile.name))?;
        let create_result = run_borg_create(profile, &archive, mount.path());
        let unmount_result = mount.unmount();

        if let Err(error) = create_result {
            if let Err(unmount_error) = unmount_result {
                eprintln!("{unmount_error:#}");
            }
            return Err(error);
        }
        if let Err(error) = unmount_result {
            report.maintenance_failed("unmount", profile, error);
        }

        report.archives_created += 1;
        eprintln!(
            "created borg archive profile={} repo={} archive={}",
            profile.name, profile.repo, archive
        );

        if let Some(prune) = &profile.prune {
            if let Err(error) = run_borg(prune_command(profile, prune), "borg prune", profile) {
                report.maintenance_failed("prune", profile, error);
            } else if let Err(error) = run_borg(compact_command(profile), "borg compact", profile) {
                report.maintenance_failed("compact", profile, error);
            }
        }
    }

    Ok(report)
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

/// Create the mount directory with mode 0700. Snapshots are usually only
/// readable by root, and the bind mounts must not expose them to other users.
fn prepare_mount_dir(mount_dir: &Path) -> Result<PathBuf> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(mount_dir)
        .with_context(|| format!("failed to create mount directory {}", mount_dir.display()))?;
    fs::set_permissions(mount_dir, fs::Permissions::from_mode(0o700)).with_context(|| {
        format!(
            "failed to set permissions on mount directory {}",
            mount_dir.display()
        )
    })?;
    // mountinfo shows canonical paths, so compare against the canonical form.
    fs::canonicalize(mount_dir)
        .with_context(|| format!("failed to resolve mount directory {}", mount_dir.display()))
}

/// A bind mount of a snapshot at a stable path. borg's files cache is keyed
/// by the absolute path, so a stable path lets it skip unchanged files.
struct SnapshotMount {
    path: PathBuf,
    mounted: bool,
}

impl SnapshotMount {
    fn mount(source: &Path, target: &Path) -> Result<Self> {
        fs::create_dir_all(target)
            .with_context(|| format!("failed to create mount point {}", target.display()))?;

        let mut attempts = 0;
        while is_mount_point(target)? {
            if attempts == MAX_LEFTOVER_UNMOUNTS {
                bail!("could not remove leftover mounts from {}", target.display());
            }
            eprintln!("removing leftover mount path={}", target.display());
            run_command(umount_command(target), "umount")?;
            attempts += 1;
        }

        eprintln!(
            "mounting snapshot source={} target={}",
            source.display(),
            target.display()
        );
        run_command(mount_command(source, target), "mount --bind")?;

        Ok(Self {
            path: target.to_path_buf(),
            mounted: true,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn unmount(mut self) -> Result<()> {
        self.mounted = false;
        run_command(umount_command(&self.path), "umount")
            .with_context(|| format!("failed to unmount {}", self.path.display()))
    }
}

impl Drop for SnapshotMount {
    fn drop(&mut self) {
        if self.mounted
            && let Err(error) = run_command(umount_command(&self.path), "umount")
        {
            eprintln!("failed to unmount {}: {error:#}", self.path.display());
        }
    }
}

fn is_mount_point(path: &Path) -> Result<bool> {
    let mountinfo = fs::read_to_string("/proc/self/mountinfo")
        .context("failed to read /proc/self/mountinfo")?;
    Ok(mountinfo_contains(&mountinfo, path))
}

/// Check if `path` is a mount point in the given `/proc/self/mountinfo` text.
fn mountinfo_contains(mountinfo: &str, path: &Path) -> bool {
    let wanted = path.as_os_str().as_encoded_bytes();
    mountinfo
        .lines()
        .filter_map(|line| line.split(' ').nth(4))
        .any(|mount_point| unescape_mountinfo(mount_point) == wanted)
}

/// mountinfo escapes space, tab, newline and backslash as `\ooo` (octal).
fn unescape_mountinfo(field: &str) -> Vec<u8> {
    let bytes = field.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\'
            && index + 3 < bytes.len()
            && bytes[index + 1..index + 4]
                .iter()
                .all(|b| (b'0'..=b'7').contains(b))
        {
            let value = (bytes[index + 1] - b'0') as u32 * 64
                + (bytes[index + 2] - b'0') as u32 * 8
                + (bytes[index + 3] - b'0') as u32;
            result.push(value as u8);
            index += 4;
        } else {
            result.push(bytes[index]);
            index += 1;
        }
    }
    result
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

fn run_borg_create(profile: &ProfileConfig, archive: &str, mount_path: &Path) -> Result<()> {
    eprintln!(
        "running borg create profile={} repo={} archive={} path={}",
        profile.name,
        profile.repo,
        archive,
        mount_path.display()
    );
    run_borg(
        create_command(profile, archive, mount_path),
        "borg create",
        profile,
    )
}

fn run_borg(command: Command, what: &str, profile: &ProfileConfig) -> Result<()> {
    run_command(command, what)
        .with_context(|| format!("{what} failed for profile {}", profile.name))
}

/// Run a command with stdout and stderr passed through to the journal.
fn run_command(mut command: Command, what: &str) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("failed to invoke {what}"))?;
    if !status.success() {
        bail!("{what} exited with {status}");
    }
    Ok(())
}

/// `borg create` runs inside the mounted snapshot with `.` as the root, so
/// archived paths and exclude patterns are relative to the subvolume root.
fn create_command(profile: &ProfileConfig, archive: &str, mount_path: &Path) -> Command {
    let mut command = Command::new("borg");
    command.arg("create");
    command.args(&profile.extra_args);
    for pattern in &profile.exclude {
        command.arg("--exclude").arg(pattern);
    }
    command.arg(format!("{}::{archive}", profile.repo));
    command.arg(".");
    command.current_dir(mount_path);
    command.env("BORG_PASSPHRASE", &profile.passphrase);
    command
}

fn prune_command(profile: &ProfileConfig, prune: &PruneConfig) -> Command {
    let mut command = Command::new("borg");
    command.arg("prune");
    command.arg("--list");
    command.arg("--glob-archives");
    command.arg(format!("{}-*", profile.archive_prefix));
    command.args(prune.borg_args());
    command.arg(&profile.repo);
    command.env("BORG_PASSPHRASE", &profile.passphrase);
    command
}

fn compact_command(profile: &ProfileConfig) -> Command {
    let mut command = Command::new("borg");
    command.arg("compact");
    command.arg(&profile.repo);
    command.env("BORG_PASSPHRASE", &profile.passphrase);
    command
}

fn mount_command(source: &Path, target: &Path) -> Command {
    let mut command = Command::new("mount");
    command.arg("--bind").arg(source).arg(target);
    command
}

fn umount_command(target: &Path) -> Command {
    let mut command = Command::new("umount");
    command.arg(target);
    command
}

fn create_archive_name(prefix: &str, timestamp: chrono::DateTime<Utc>) -> String {
    format!("{}-{}", prefix, timestamp.format("%Y-%m-%dT%H-%M-%SZ"))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::Path;
    use std::process::Command;

    use super::{
        BackupConfig, DEFAULT_LOCK_PATH, DEFAULT_MOUNT_DIR, DEFAULT_STATE_PATH, compact_command,
        create_command, mount_command, mountinfo_contains, prune_command, umount_command,
    };

    const PROFILE: &str = r#"
        [[profile]]
        name = "root"
        snapper_config = "root"
        repo = "repo"
        passphrase = "secret"
        archive_prefix = "host-root"
    "#;

    fn parse(toml: &str) -> BackupConfig {
        toml::from_str(toml).unwrap()
    }

    fn validation_error(toml: &str) -> String {
        format!("{:#}", parse(toml).validate().unwrap_err())
    }

    fn args(command: &Command) -> Vec<&OsStr> {
        command.get_args().collect()
    }

    fn passphrase(command: &Command) -> Option<&OsStr> {
        command
            .get_envs()
            .find(|(key, _)| *key == "BORG_PASSPHRASE")
            .and_then(|(_, value)| value)
    }

    #[test]
    fn parses_minimal_backend_config_with_defaults() {
        let config = parse(PROFILE);

        config.validate().unwrap();

        assert_eq!(config.state_path(), Path::new(DEFAULT_STATE_PATH));
        assert_eq!(config.lock_path(), Path::new(DEFAULT_LOCK_PATH));
        assert_eq!(config.mount_dir(), Path::new(DEFAULT_MOUNT_DIR));
        assert_eq!(config.profiles.len(), 1);
        assert!(config.profiles[0].exclude.is_empty());
        assert!(config.profiles[0].prune.is_none());
    }

    #[test]
    fn ignores_frontend_only_config_sections() {
        let config = parse(&format!(
            r#"
                [general]
                state_path = "/tmp/state.json"
                service_name = "custom.service"

                [status]
                warn_after_hours = 12
                stale_after_hours = 24
                poll_seconds = 15
                {PROFILE}
            "#
        ));

        config.validate().unwrap();
        assert_eq!(config.state_path(), Path::new("/tmp/state.json"));
    }

    #[test]
    fn example_config_is_valid() {
        parse(include_str!("../contrib/config.toml.example"))
            .validate()
            .unwrap();
    }

    #[test]
    fn parses_mount_dir_override() {
        let config = parse(&format!("[general]\nmount_dir = \"/tmp/mnt\"\n{PROFILE}"));
        config.validate().unwrap();
        assert_eq!(config.mount_dir(), Path::new("/tmp/mnt"));
    }

    #[test]
    fn rejects_empty_mount_dir() {
        let error = validation_error(&format!("[general]\nmount_dir = \"\"\n{PROFILE}"));
        assert!(error.contains("general.mount_dir"), "{error}");
    }

    #[test]
    fn parses_exclude_and_prune_table() {
        let config = parse(&format!(
            r#"{PROFILE}
                exclude = ["home/*/.cache", "sh:**/node_modules"]

                [profile.prune]
                keep_within = "2d"
                keep_daily = 7
                keep_weekly = 4
            "#
        ));
        config.validate().unwrap();

        let profile = &config.profiles[0];
        assert_eq!(profile.exclude, ["home/*/.cache", "sh:**/node_modules"]);
        let prune = profile.prune.as_ref().unwrap();
        assert_eq!(
            prune.borg_args(),
            [
                "--keep-within",
                "2d",
                "--keep-daily",
                "7",
                "--keep-weekly",
                "4"
            ]
        );
    }

    #[test]
    fn maps_all_prune_rules_to_borg_flags() {
        let config = parse(&format!(
            r#"{PROFILE}
                [profile.prune]
                keep_last = 1
                keep_minutely = 2
                keep_hourly = 3
                keep_daily = 4
                keep_weekly = 5
                keep_monthly = 6
                keep_13weekly = 7
                keep_3monthly = 8
                keep_yearly = 9
            "#
        ));
        config.validate().unwrap();

        assert_eq!(
            config.profiles[0].prune.as_ref().unwrap().borg_args(),
            [
                "--keep-last",
                "1",
                "--keep-minutely",
                "2",
                "--keep-hourly",
                "3",
                "--keep-daily",
                "4",
                "--keep-weekly",
                "5",
                "--keep-monthly",
                "6",
                "--keep-13weekly",
                "7",
                "--keep-3monthly",
                "8",
                "--keep-yearly",
                "9"
            ]
        );
    }

    #[test]
    fn rejects_unknown_prune_key() {
        let result: Result<BackupConfig, _> =
            toml::from_str(&format!("{PROFILE}\n[profile.prune]\nkeep_dayly = 7\n"));
        let error = result.unwrap_err().to_string();
        assert!(error.contains("keep_dayly"), "{error}");
    }

    #[test]
    fn rejects_prune_table_without_rules() {
        let error = validation_error(&format!("{PROFILE}\n[profile.prune]\n"));
        assert!(error.contains("at least one keep_* rule"), "{error}");
    }

    #[test]
    fn rejects_zero_and_other_negative_prune_counts() {
        for value in ["0", "-2"] {
            let error = validation_error(&format!(
                "{PROFILE}\n[profile.prune]\nkeep_daily = {value}\n"
            ));
            assert!(
                error.contains("prune.keep_daily must be at least 1, or -1 for no limit"),
                "{value}: {error}"
            );
        }
    }

    #[test]
    fn accepts_minus_one_as_no_limit() {
        let config = parse(&format!(
            "{PROFILE}\n[profile.prune]\nkeep_daily = 7\nkeep_yearly = -1\n"
        ));
        config.validate().unwrap();
        assert_eq!(
            config.profiles[0].prune.as_ref().unwrap().borg_args(),
            ["--keep-daily", "7", "--keep-yearly", "-1"]
        );
    }

    #[test]
    fn rejects_empty_keep_within() {
        let error = validation_error(&format!(
            "{PROFILE}\n[profile.prune]\nkeep_within = \" \"\n"
        ));
        assert!(error.contains("prune.keep_within"), "{error}");
    }

    #[test]
    fn rejects_empty_exclude_entry() {
        let error = validation_error(&format!("{PROFILE}\nexclude = [\"tmp\", \"\"]\n"));
        assert!(
            error.contains("exclude entries must not be empty"),
            "{error}"
        );
    }

    #[test]
    fn rejects_profile_names_that_are_not_directory_names() {
        for name in ["a/b", ".", ".."] {
            let error = validation_error(
                &PROFILE.replace("name = \"root\"", &format!("name = \"{name}\"")),
            );
            assert!(error.contains("directory name"), "{name}: {error}");
        }
    }

    #[test]
    fn rejects_duplicate_profile_names() {
        let second = PROFILE.replace("repo = \"repo\"", "repo = \"other\"");
        let error = validation_error(&format!("{PROFILE}{second}"));
        assert!(error.contains("used more than once"), "{error}");
    }

    #[test]
    fn rejects_glob_characters_in_archive_prefix() {
        let error = validation_error(&PROFILE.replace("host-root", "host-*"));
        assert!(error.contains("glob characters"), "{error}");
    }

    fn two_profiles(first_prefix: &str, second_prefix: &str, second_repo: &str) -> String {
        format!(
            r#"
                [[profile]]
                name = "a"
                snapper_config = "a"
                repo = "repo"
                passphrase = "secret"
                archive_prefix = "{first_prefix}"

                [[profile]]
                name = "b"
                snapper_config = "b"
                repo = "{second_repo}"
                passphrase = "secret"
                archive_prefix = "{second_prefix}"
            "#
        )
    }

    #[test]
    fn rejects_overlapping_prefixes_on_same_repo() {
        for (first, second) in [
            ("host", "host"),
            ("host", "host-root"),
            ("host-root", "host"),
        ] {
            let error = validation_error(&two_profiles(first, second, "repo"));
            assert!(
                error.contains("overlapping archive_prefix"),
                "{first}/{second}: {error}"
            );
        }
    }

    #[test]
    fn accepts_similar_prefixes_that_do_not_overlap() {
        parse(&two_profiles("host-root", "host-root2", "repo"))
            .validate()
            .unwrap();
        parse(&two_profiles("host", "host-root", "other-repo"))
            .validate()
            .unwrap();
    }

    #[test]
    fn create_command_runs_in_mount_with_relative_root_and_excludes() {
        let config = parse(&format!(
            "{PROFILE}\nextra_args = [\"--stats\"]\nexclude = [\"user/.cache\", \"sh:**/target\"]\n"
        ));
        let command = create_command(
            &config.profiles[0],
            "host-root-2026",
            Path::new("/run/bogger/mnt/root"),
        );

        assert_eq!(command.get_program(), "borg");
        assert_eq!(
            args(&command),
            [
                "create",
                "--stats",
                "--exclude",
                "user/.cache",
                "--exclude",
                "sh:**/target",
                "repo::host-root-2026",
                "."
            ]
        );
        assert_eq!(
            command.get_current_dir(),
            Some(Path::new("/run/bogger/mnt/root"))
        );
        assert_eq!(passphrase(&command), Some(OsStr::new("secret")));
    }

    #[test]
    fn prune_command_limits_to_profile_archives() {
        let config = parse(&format!("{PROFILE}\n[profile.prune]\nkeep_daily = 7\n"));
        let profile = &config.profiles[0];
        let command = prune_command(profile, profile.prune.as_ref().unwrap());

        assert_eq!(
            args(&command),
            [
                "prune",
                "--list",
                "--glob-archives",
                "host-root-*",
                "--keep-daily",
                "7",
                "repo"
            ]
        );
        assert_eq!(passphrase(&command), Some(OsStr::new("secret")));
    }

    #[test]
    fn compact_command_targets_repo() {
        let config = parse(PROFILE);
        let command = compact_command(&config.profiles[0]);

        assert_eq!(args(&command), ["compact", "repo"]);
        assert_eq!(passphrase(&command), Some(OsStr::new("secret")));
    }

    #[test]
    fn mount_commands_bind_snapshot_to_target() {
        let mount = mount_command(
            Path::new("/home/.snapshots/12/snapshot"),
            Path::new("/run/bogger/mnt/home"),
        );
        assert_eq!(mount.get_program(), "mount");
        assert_eq!(
            args(&mount),
            [
                "--bind",
                "/home/.snapshots/12/snapshot",
                "/run/bogger/mnt/home"
            ]
        );

        let umount = umount_command(Path::new("/run/bogger/mnt/home"));
        assert_eq!(umount.get_program(), "umount");
        assert_eq!(args(&umount), ["/run/bogger/mnt/home"]);
    }

    #[test]
    fn detects_mount_points_in_mountinfo() {
        let mountinfo = "\
22 1 0:21 / / rw,relatime shared:1 - btrfs /dev/sda2 rw
90 22 0:45 /home/.snapshots/12/snapshot /run/bogger/mnt/home rw shared:1 - btrfs /dev/sda2 rw
91 22 0:45 /x /run/bogger/mnt/my\\040profile rw shared:1 - btrfs /dev/sda2 rw
";
        assert!(mountinfo_contains(
            mountinfo,
            Path::new("/run/bogger/mnt/home")
        ));
        assert!(mountinfo_contains(
            mountinfo,
            Path::new("/run/bogger/mnt/my profile")
        ));
        assert!(!mountinfo_contains(
            mountinfo,
            Path::new("/run/bogger/mnt/root")
        ));
        assert!(!mountinfo_contains(mountinfo, Path::new("/run/bogger/mnt")));
    }
}
