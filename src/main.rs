mod backup;
mod state;
mod systemd;
mod watch;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use backup::{BackupConfig, DEFAULT_CONFIG_PATH};
use clap::{Parser, Subcommand};
use watch::DerivedStatus;

pub const DEFAULT_STATE_PATH: &str = "/var/lib/borg-reminder/state.json";
pub const DEFAULT_SERVICE_NAME: &str = "borg-reminder-backup.service";
pub const DEFAULT_POLL_SECONDS: u64 = 15;
pub const DEFAULT_WARN_AFTER_HOURS: u64 = 12;
pub const DEFAULT_STALE_AFTER_HOURS: u64 = 24;

#[derive(Parser, Debug)]
#[command(name = "borg-reminder")]
#[command(about = "Manual Borg backup helper for ashell")]
struct Cli {
    #[arg(long, global = true, default_value = DEFAULT_STATE_PATH)]
    state_path: PathBuf,

    #[arg(long, global = true, default_value = DEFAULT_SERVICE_NAME)]
    service_name: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Watch {
        #[arg(long, default_value_t = DEFAULT_POLL_SECONDS)]
        poll_seconds: u64,

        #[arg(long, default_value_t = DEFAULT_WARN_AFTER_HOURS)]
        warn_after_hours: u64,

        #[arg(long, default_value_t = DEFAULT_STALE_AFTER_HOURS)]
        stale_after_hours: u64,
    },
    Trigger,
    Backup {
        #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
        config: PathBuf,
    },
    Status {
        #[arg(long)]
        json: bool,

        #[arg(long, default_value_t = DEFAULT_WARN_AFTER_HOURS)]
        warn_after_hours: u64,

        #[arg(long, default_value_t = DEFAULT_STALE_AFTER_HOURS)]
        stale_after_hours: u64,
    },
    Pause,
    Resume,
}

fn main() -> Result<()> {
    let Cli {
        state_path,
        service_name,
        command,
    } = Cli::parse();

    match command {
        Commands::Watch {
            poll_seconds,
            warn_after_hours,
            stale_after_hours,
        } => run_watch_command(
            state_path.as_path(),
            service_name.as_str(),
            poll_seconds,
            warn_after_hours,
            stale_after_hours,
        ),
        Commands::Trigger => run_trigger_command(service_name.as_str()),
        Commands::Backup { config } => {
            let config = BackupConfig::load(&config)
                .with_context(|| format!("failed to load config {}", config.display()))?;
            backup::run_backup(&config)
        }
        Commands::Status {
            json,
            warn_after_hours,
            stale_after_hours,
        } => run_status_command(
            state_path.as_path(),
            service_name.as_str(),
            warn_after_hours,
            stale_after_hours,
            json,
        ),
        Commands::Pause => run_pause_command(service_name.as_str()),
        Commands::Resume => run_resume_command(service_name.as_str()),
    }
}

fn run_watch_command(
    state_path: &Path,
    service_name: &str,
    poll_seconds: u64,
    warn_after_hours: u64,
    stale_after_hours: u64,
) -> Result<()> {
    validate_service_name(service_name)?;
    validate_poll_seconds(poll_seconds)?;
    validate_status_thresholds(warn_after_hours, stale_after_hours)?;
    watch::run_watch(
        state_path,
        service_name,
        poll_seconds,
        warn_after_hours,
        stale_after_hours,
    )
}

fn run_trigger_command(service_name: &str) -> Result<()> {
    validate_service_name(service_name)?;
    systemd::trigger_backup(service_name)
}

fn run_pause_command(service_name: &str) -> Result<()> {
    validate_service_name(service_name)?;
    systemd::pause_backup(service_name)
}

fn run_resume_command(service_name: &str) -> Result<()> {
    validate_service_name(service_name)?;
    systemd::resume_backup(service_name)
}

fn validate_service_name(service_name: &str) -> Result<()> {
    if service_name.trim().is_empty() {
        bail!("service_name must not be empty");
    }

    Ok(())
}

fn validate_poll_seconds(poll_seconds: u64) -> Result<()> {
    if poll_seconds == 0 {
        bail!("status.poll_seconds must be greater than zero");
    }

    Ok(())
}

fn validate_status_thresholds(warn_after_hours: u64, stale_after_hours: u64) -> Result<()> {
    if warn_after_hours > stale_after_hours {
        bail!("status.warn_after_hours must be less than or equal to status.stale_after_hours");
    }

    Ok(())
}

fn run_status_command(
    state_path: &Path,
    service_name: &str,
    warn_after_hours: u64,
    stale_after_hours: u64,
    json: bool,
) -> Result<()> {
    validate_service_name(service_name)?;
    validate_status_thresholds(warn_after_hours, stale_after_hours)?;

    if !json {
        bail!("status currently only supports --json");
    }

    let state = state::StateStore::new(state_path.to_path_buf()).load()?;
    let unit = systemd::query_unit_state(service_name)?;
    let derived = watch::derive_status(
        &state,
        Some(&unit),
        chrono::Utc::now(),
        warn_after_hours,
        stale_after_hours,
    );

    print_status_json(&derived)?;
    Ok(())
}

fn print_status_json(derived: &DerivedStatus) -> Result<()> {
    let output = serde_json::to_string_pretty(derived)?;
    println!("{output}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{validate_poll_seconds, validate_service_name, validate_status_thresholds};

    #[test]
    fn rejects_empty_service_name() {
        let error = validate_service_name("   ").unwrap_err();
        assert!(error.to_string().contains("service_name must not be empty"));
    }

    #[test]
    fn rejects_zero_poll_seconds() {
        let error = validate_poll_seconds(0).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("status.poll_seconds must be greater than zero")
        );
    }

    #[test]
    fn rejects_invalid_status_thresholds() {
        let error = validate_status_thresholds(25, 24).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("status.warn_after_hours must be less than or equal")
        );
    }
}
