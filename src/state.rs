use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct State {
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub last_run_result: Option<LastRunResult>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LastRunResult {
    Success,
    Error,
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
            ..State::default()
        };

        let json = serde_json::to_string(&state).unwrap();
        let decoded: State = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.last_run_result, Some(LastRunResult::Success));
    }

    #[test]
    fn loads_legacy_state_with_extra_fields() {
        let decoded: State = serde_json::from_str(
            r#"{
                "last_success_at": null,
                "last_attempt_at": null,
                "started_at": null,
                "last_run_result": "error",
                "last_error": "old detail",
                "active_job_id": 1234,
                "archives_created": [],
                "snapshots_used": []
            }"#,
        )
        .unwrap();

        assert_eq!(decoded.last_run_result, Some(LastRunResult::Error));
    }
}
