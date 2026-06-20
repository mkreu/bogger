use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::config::Config;

#[derive(Debug, Clone, Serialize)]
pub struct UnitState {
    pub active_state: String,
    pub sub_state: String,
    pub exec_main_pid: Option<u32>,
    pub process_state: Option<String>,
}

impl UnitState {
    pub fn is_active(&self) -> bool {
        self.active_state == "active" || self.active_state == "activating"
    }

    pub fn is_paused(&self) -> bool {
        matches!(self.process_state.as_deref(), Some("T") | Some("t"))
    }
}

pub fn trigger_backup(config: &Config) -> Result<()> {
    let unit = query_unit_state_optional(config.service_name());
    if let Some(unit) = unit {
        if unit.is_active() {
            return Ok(());
        }
    }

    run_pkexec_systemctl(&["start", config.service_name()])
}

pub fn pause_backup(config: &Config) -> Result<()> {
    run_pkexec_systemctl(&["kill", "-s", "STOP", config.service_name()])
}

pub fn resume_backup(config: &Config) -> Result<()> {
    run_pkexec_systemctl(&["kill", "-s", "CONT", config.service_name()])
}

pub fn query_unit_state_optional(service_name: &str) -> Option<UnitState> {
    query_unit_state(service_name).ok()
}

pub fn query_unit_state(service_name: &str) -> Result<UnitState> {
    let output = Command::new("systemctl")
        .args([
            "show",
            service_name,
            "--property=ActiveState,SubState,ExecMainPID",
            "--value",
        ])
        .output()
        .with_context(|| format!("failed to invoke systemctl for {service_name}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("systemctl show failed for {service_name}: {stderr}");
    }

    let stdout = String::from_utf8(output.stdout)?;
    let mut lines = stdout.lines();
    let active_state = lines.next().unwrap_or_default().trim().to_string();
    let sub_state = lines.next().unwrap_or_default().trim().to_string();
    let exec_main_pid = lines
        .next()
        .and_then(|line| line.trim().parse::<u32>().ok())
        .filter(|pid| *pid != 0);

    let process_state = exec_main_pid.and_then(read_process_state);

    Ok(UnitState {
        active_state,
        sub_state,
        exec_main_pid,
        process_state,
    })
}

fn run_pkexec_systemctl(args: &[&str]) -> Result<()> {
    let output = Command::new("pkexec")
        .arg("systemctl")
        .args(args)
        .output()
        .with_context(|| "failed to invoke pkexec systemctl")?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!("pkexec systemctl {} failed: {stderr}", args.join(" "));
}

fn read_process_state(pid: u32) -> Option<String> {
    let path = Path::new("/proc").join(pid.to_string()).join("status");
    let contents = fs::read_to_string(path).ok()?;
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("State:") {
            let code = rest.split_whitespace().next()?.to_string();
            return Some(code);
        }
    }
    None
}
