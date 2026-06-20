use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

pub const DEFAULT_CONFIG_PATH: &str = "/etc/borg-reminder/config.toml";
const DEFAULT_STATE_PATH: &str = "/var/lib/borg-reminder/state.json";
const DEFAULT_LOCK_PATH: &str = "/run/borg-reminder/lock";
const DEFAULT_SERVICE_NAME: &str = "borg-reminder-backup.service";
const DEFAULT_POLL_SECONDS: u64 = 15;
const DEFAULT_WARN_AFTER_HOURS: u64 = 12;
const DEFAULT_STALE_AFTER_HOURS: u64 = 24;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    general: GeneralConfig,
    #[serde(default)]
    pub status: StatusConfig,
    #[serde(default, rename = "profile")]
    pub profiles: Vec<ProfileConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct GeneralConfig {
    state_path: Option<PathBuf>,
    lock_path: Option<PathBuf>,
    service_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StatusConfig {
    #[serde(default = "default_warn_after_hours")]
    pub warn_after_hours: u64,
    #[serde(default = "default_stale_after_hours")]
    pub stale_after_hours: u64,
    #[serde(default = "default_poll_seconds")]
    pub poll_seconds: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProfileConfig {
    pub name: String,
    pub snapper_config: String,
    pub repo: String,
    pub passphrase: String,
    pub archive_prefix: String,
    #[serde(default)]
    pub extra_args: Vec<String>,
}

impl Default for StatusConfig {
    fn default() -> Self {
        Self {
            warn_after_hours: default_warn_after_hours(),
            stale_after_hours: default_stale_after_hours(),
            poll_seconds: default_poll_seconds(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read config {}", path.display()))?;
        let config: Config = toml::from_str(&contents)
            .with_context(|| format!("failed to parse config {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn state_path(&self) -> &Path {
        self.general
            .state_path
            .as_deref()
            .unwrap_or_else(|| Path::new(DEFAULT_STATE_PATH))
    }

    pub fn lock_path(&self) -> &Path {
        self.general
            .lock_path
            .as_deref()
            .unwrap_or_else(|| Path::new(DEFAULT_LOCK_PATH))
    }

    pub fn service_name(&self) -> &str {
        self.general
            .service_name
            .as_deref()
            .unwrap_or(DEFAULT_SERVICE_NAME)
    }

    fn validate(&self) -> Result<()> {
        if self.status.poll_seconds == 0 {
            bail!("status.poll_seconds must be greater than zero");
        }

        if self.status.warn_after_hours > self.status.stale_after_hours {
            bail!("status.warn_after_hours must be less than or equal to status.stale_after_hours");
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

fn default_warn_after_hours() -> u64 {
    DEFAULT_WARN_AFTER_HOURS
}

fn default_stale_after_hours() -> u64 {
    DEFAULT_STALE_AFTER_HOURS
}

fn default_poll_seconds() -> u64 {
    DEFAULT_POLL_SECONDS
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn parses_minimal_config_with_defaults() {
        let config: Config = toml::from_str(
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

        assert_eq!(config.service_name(), "borg-reminder-backup.service");
        assert_eq!(config.status.poll_seconds, 15);
        assert_eq!(config.profiles.len(), 1);
    }
}
