use std::path::Path;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::Serialize;

use crate::state::{LastRunResult, State, StateStore};
use crate::systemd::UnitState;

#[derive(Debug, Clone, Serialize)]
pub struct DerivedStatus {
    pub text: String,
    pub alt: String,
    pub warning: bool,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub last_run_result: Option<LastRunResult>,
    pub unit: Option<UnitState>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct WatchOutput<'a> {
    text: &'a str,
    alt: &'a str,
}

pub fn run_watch(
    state_path: &Path,
    service_name: &str,
    poll_seconds: u64,
    warn_after_hours: u64,
    stale_after_hours: u64,
) -> Result<()> {
    let store = StateStore::new(state_path.to_path_buf());
    let mut last_output = None;

    loop {
        let state = store.load()?;
        let unit = crate::systemd::query_unit_state(service_name)?;
        let derived = derive_status(
            &state,
            Some(&unit),
            Utc::now(),
            warn_after_hours,
            stale_after_hours,
        );
        let output = serde_json::to_string(&WatchOutput {
            text: &derived.text,
            alt: &derived.alt,
        })?;

        if last_output.as_deref() != Some(output.as_str()) {
            println!("{output}");
            last_output = Some(output);
        }

        thread::sleep(Duration::from_secs(poll_seconds));
    }
}

pub fn derive_status(
    state: &State,
    unit: Option<&UnitState>,
    now: DateTime<Utc>,
    warn_after_hours: u64,
    stale_after_hours: u64,
) -> DerivedStatus {
    let warning_cutoff = ChronoDuration::hours(warn_after_hours as i64);
    let stale_cutoff = ChronoDuration::hours(stale_after_hours as i64);

    let (alt, text) = if let Some(unit) = unit {
        if unit.is_active() {
            if unit.is_paused() {
                ("paused".to_string(), "paused".to_string())
            } else {
                ("running".to_string(), "running".to_string())
            }
        } else {
            derive_inactive_status(state, now, stale_cutoff)
        }
    } else {
        derive_inactive_status(state, now, stale_cutoff)
    };

    let warning = state
        .last_success_at
        .map(|last_success| now - last_success >= warning_cutoff)
        .unwrap_or(true);

    DerivedStatus {
        text,
        alt,
        warning,
        last_success_at: state.last_success_at,
        last_attempt_at: state.last_attempt_at,
        started_at: state.started_at,
        last_run_result: state.last_run_result,
        unit: unit.cloned(),
    }
}

fn derive_inactive_status(
    state: &State,
    now: DateTime<Utc>,
    stale_cutoff: ChronoDuration,
) -> (String, String) {
    let last_attempt_after_success = match (state.last_attempt_at, state.last_success_at) {
        (Some(last_attempt), Some(last_success)) => last_attempt > last_success,
        (Some(_), None) => true,
        _ => false,
    };

    if state.last_run_result == Some(LastRunResult::Error) && last_attempt_after_success {
        return ("error".to_string(), "failed".to_string());
    }

    match state.last_success_at {
        Some(last_success) if now - last_success >= stale_cutoff => {
            ("stale".to_string(), format_elapsed(now - last_success))
        }
        Some(last_success) => ("idle".to_string(), format_elapsed(now - last_success)),
        None => ("stale".to_string(), "never".to_string()),
    }
}

pub fn format_elapsed(elapsed: ChronoDuration) -> String {
    let seconds = elapsed.num_seconds().max(0);
    if seconds < 60 {
        return format!("{seconds}s");
    }

    let minutes = elapsed.num_minutes();
    if minutes < 60 {
        return format!("{minutes}m");
    }

    let hours = elapsed.num_hours();
    if hours < 48 {
        return format!("{hours}h");
    }

    format!("{}d", elapsed.num_days())
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone, Utc};

    use crate::state::{LastRunResult, State};

    use super::{derive_status, format_elapsed};

    #[test]
    fn formats_elapsed_time() {
        assert_eq!(format_elapsed(Duration::seconds(45)), "45s");
        assert_eq!(format_elapsed(Duration::minutes(9)), "9m");
        assert_eq!(format_elapsed(Duration::hours(5)), "5h");
        assert_eq!(format_elapsed(Duration::days(3)), "3d");
    }

    #[test]
    fn marks_old_backup_as_stale() {
        let now = Utc.with_ymd_and_hms(2026, 6, 20, 12, 0, 0).unwrap();
        let state = State {
            last_success_at: Some(now - Duration::hours(30)),
            ..State::default()
        };

        let derived = derive_status(&state, None, now, 12, 24);
        assert_eq!(derived.alt, "stale");
        assert_eq!(derived.text, "30h");
    }

    #[test]
    fn keeps_failure_visible_until_next_success() {
        let now = Utc.with_ymd_and_hms(2026, 6, 20, 12, 0, 0).unwrap();
        let state = State {
            last_success_at: Some(now - Duration::hours(2)),
            last_attempt_at: Some(now - Duration::hours(1)),
            last_run_result: Some(LastRunResult::Error),
            ..State::default()
        };

        let derived = derive_status(&state, None, now, 12, 24);
        assert_eq!(derived.alt, "error");
        assert_eq!(derived.text, "failed");
    }
}
