mod backup;
mod state;
mod systemd;
mod watch;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use backup::{BackupConfig, DEFAULT_CONFIG_PATH};
use clap::{Args, Parser, Subcommand};

pub const DEFAULT_STATE_PATH: &str = "/var/lib/bogger/state.json";
pub const DEFAULT_SERVICE_NAME: &str = "bogger-backup.service";
pub const DEFAULT_POLL_SECONDS: u64 = 15;
pub const DEFAULT_WARN_AFTER_HOURS: u64 = 12;
pub const DEFAULT_STALE_AFTER_HOURS: u64 = 24;

#[derive(Parser, Debug)]
#[command(name = "bogger")]
#[command(about = "Manual Borg backup helper for ashell")]
struct Cli {
    #[arg(long, global = true, default_value = DEFAULT_STATE_PATH)]
    state_path: PathBuf,

    #[arg(long, global = true, default_value = DEFAULT_SERVICE_NAME, value_parser = parse_service_name)]
    service_name: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Watch(WatchArgs),
    Trigger,
    Backup {
        #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
        config: PathBuf,
    },
    Status,
    Pause,
    Resume,
}

#[derive(Args, Debug)]
struct WatchArgs {
    #[arg(long, default_value_t = DEFAULT_POLL_SECONDS, value_parser = parse_poll_seconds)]
    poll_seconds: u64,

    #[arg(long, default_value_t = DEFAULT_WARN_AFTER_HOURS)]
    warn_after_hours: u64,

    #[arg(long, default_value_t = DEFAULT_STALE_AFTER_HOURS)]
    stale_after_hours: u64,
}

fn main() -> Result<()> {
    Cli::parse().run()
}

impl Cli {
    fn run(self) -> Result<()> {
        match self.command {
            Commands::Watch(args) => {
                args.validate()?;
                watch::run_watch(
                    self.state_path.as_path(),
                    self.service_name.as_str(),
                    args.poll_seconds,
                    args.warn_after_hours,
                    args.stale_after_hours,
                )
            }
            Commands::Trigger => systemd::trigger_backup(self.service_name.as_str()),
            Commands::Backup { config } => {
                let config = BackupConfig::load(&config)
                    .with_context(|| format!("failed to load config {}", config.display()))?;
                backup::run_backup(&config)
            }
            Commands::Status => {
                run_status_command(self.state_path.as_path(), self.service_name.as_str())
            }
            Commands::Pause => systemd::pause_backup(self.service_name.as_str()),
            Commands::Resume => systemd::resume_backup(self.service_name.as_str()),
        }
    }
}

impl WatchArgs {
    fn validate(&self) -> Result<()> {
        if self.warn_after_hours > self.stale_after_hours {
            bail!("status.warn_after_hours must be less than or equal to status.stale_after_hours");
        }

        Ok(())
    }
}

fn parse_service_name(service_name: &str) -> Result<String, String> {
    if service_name.trim().is_empty() {
        return Err("service_name must not be empty".to_owned());
    }

    Ok(service_name.to_owned())
}

fn parse_poll_seconds(poll_seconds: &str) -> Result<u64, String> {
    let poll_seconds = poll_seconds
        .parse::<u64>()
        .map_err(|error| error.to_string())?;

    if poll_seconds == 0 {
        return Err("status.poll_seconds must be greater than zero".to_owned());
    }

    Ok(poll_seconds)
}

fn run_status_command(state_path: &Path, service_name: &str) -> Result<()> {
    let state = state::StateStore::new(state_path.to_path_buf()).load()?;
    let unit = systemd::query_unit_state(service_name)?;
    let derived = watch::derive_status(
        &state,
        Some(&unit),
        chrono::Utc::now(),
        DEFAULT_WARN_AFTER_HOURS,
        DEFAULT_STALE_AFTER_HOURS,
    );

    println!("{}", derived.alt);
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Cli, Commands, WatchArgs};

    #[test]
    fn rejects_empty_service_name() {
        let error =
            Cli::try_parse_from(["bogger", "--service-name", "   ", "status"]).unwrap_err();
        assert!(error.to_string().contains("service_name must not be empty"));
    }

    #[test]
    fn rejects_zero_poll_seconds() {
        let error =
            Cli::try_parse_from(["bogger", "watch", "--poll-seconds", "0"]).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("status.poll_seconds must be greater than zero")
        );
    }

    #[test]
    fn rejects_invalid_status_thresholds() {
        let error = WatchArgs {
            poll_seconds: 1,
            warn_after_hours: 25,
            stale_after_hours: 24,
        }
        .validate()
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("status.warn_after_hours must be less than or equal")
        );
    }

    #[test]
    fn parses_status_without_flags() {
        let cli = Cli::try_parse_from(["bogger", "status"]).unwrap();
        assert!(matches!(cli.command, Commands::Status));
    }

    #[test]
    fn parses_watch_without_flags() {
        let cli = Cli::try_parse_from(["bogger", "watch"]).unwrap();
        assert!(matches!(cli.command, Commands::Watch(_)));
    }

    #[test]
    fn rejects_removed_status_json_flag() {
        let error = Cli::try_parse_from(["bogger", "status", "--json"]).unwrap_err();
        assert!(error.to_string().contains("--json"));
    }

    #[test]
    fn rejects_removed_status_threshold_flags() {
        let error = Cli::try_parse_from(["bogger", "status", "--warn-after-hours", "1"])
            .unwrap_err();
        assert!(error.to_string().contains("--warn-after-hours"));

        let error = Cli::try_parse_from(["bogger", "status", "--stale-after-hours", "1"])
            .unwrap_err();
        assert!(error.to_string().contains("--stale-after-hours"));
    }
}
