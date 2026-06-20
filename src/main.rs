mod backup;
mod config;
mod state;
mod systemd;
mod watch;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use config::{Config, DEFAULT_CONFIG_PATH};
use watch::DerivedStatus;

#[derive(Parser, Debug)]
#[command(name = "borg-reminder")]
#[command(about = "Manual Borg backup helper for ashell")]
struct Cli {
    #[arg(long, global = true, default_value = DEFAULT_CONFIG_PATH)]
    config: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Watch,
    Trigger,
    Backup,
    Status {
        #[arg(long)]
        json: bool,
    },
    Pause,
    Resume,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = Config::load(&cli.config)
        .with_context(|| format!("failed to load config {}", cli.config.display()))?;

    match cli.command {
        Commands::Watch => watch::run_watch(&config),
        Commands::Trigger => systemd::trigger_backup(&config),
        Commands::Backup => backup::run_backup(&config),
        Commands::Status { json } => run_status(&config, json),
        Commands::Pause => systemd::pause_backup(&config),
        Commands::Resume => systemd::resume_backup(&config),
    }
}

fn run_status(config: &Config, json: bool) -> Result<()> {
    let state = state::StateStore::new(config.state_path().to_path_buf()).load()?;
    let unit = systemd::query_unit_state_optional(config.service_name());
    let derived = watch::derive_status(config, &state, unit.as_ref(), chrono::Utc::now());

    if !json {
        bail!("status currently only supports --json");
    }

    print_status_json(&derived)?;
    Ok(())
}

fn print_status_json(derived: &DerivedStatus) -> Result<()> {
    let output = serde_json::to_string_pretty(derived)?;
    println!("{output}");
    Ok(())
}
