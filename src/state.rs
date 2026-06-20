use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct State {
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub last_run_result: Option<LastRunResult>,
    pub active_job_id: Option<u32>,
    #[serde(default)]
    pub archives_created: Vec<ArchiveRecord>,
    #[serde(default)]
    pub snapshots_used: Vec<SnapshotRecord>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LastRunResult {
    Success,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveRecord {
    pub profile: String,
    pub archive: String,
    pub repo: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotRecord {
    pub profile: String,
    pub snapper_config: String,
    pub snapshot_number: u64,
    pub snapshot_path: PathBuf,
}

pub struct StateStore {
    path: PathBuf,
}

impl StateStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn load(&self) -> Result<State> {
        match fs::read_to_string(&self.path) {
            Ok(contents) => {
                let state = serde_json::from_str(&contents)
                    .with_context(|| format!("failed to parse state {}", self.path.display()))?;
                Ok(state)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
            Err(error) => {
                Err(error).with_context(|| format!("failed to read state {}", self.path.display()))
            }
        }
    }

    pub fn save(&self, state: &State) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create state directory {}", parent.display())
            })?;
        }

        let tmp_path = self.path.with_extension("json.tmp");
        let mut file = fs::File::create(&tmp_path)
            .with_context(|| format!("failed to create temp state {}", tmp_path.display()))?;

        serde_json::to_writer_pretty(&mut file, state)?;
        file.write_all(b"\n")?;
        file.sync_all()?;

        fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o644))
            .with_context(|| format!("failed to set permissions on {}", tmp_path.display()))?;

        fs::rename(&tmp_path, &self.path)
            .with_context(|| format!("failed to replace state file {}", self.path.display()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{LastRunResult, State};

    #[test]
    fn round_trips_state() {
        let state = State {
            last_run_result: Some(LastRunResult::Success),
            last_error: Some("none".into()),
            ..State::default()
        };

        let json = serde_json::to_string(&state).unwrap();
        let decoded: State = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.last_run_result, Some(LastRunResult::Success));
        assert_eq!(decoded.last_error.as_deref(), Some("none"));
    }
}
