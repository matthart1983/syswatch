//! Service discovery: launchctl on macOS, systemctl on Linux.
//!
//! macOS: `launchctl list` (no sudo) gives PID / Status / Label tab-separated.
//! - PID `-` means not currently running.
//! - Status `0` is normal exit (or never run); negative is killed-by-signal
//!   (-9 = SIGKILL, very common during system relaunch cycles); positive is
//!   the service's own exit code.
//!
//! Linux: `systemctl list-units --type=service --all --no-legend --plain
//! --no-pager` returns UNIT, LOAD, ACTIVE, SUB, DESCRIPTION whitespace-
//! separated.
//!
//! Cached at the slow-loop cadence (5s) — service state changes infrequently
//! and the launchctl subprocess is heavy enough to skip per-tick.

use std::time::{Duration, Instant};

use crate::collect::model::{ServiceStatus, ServiceTick};

const REFRESH: Duration = Duration::from_secs(5);

pub struct ServicesCollector {
    last_sample_at: Option<Instant>,
    cached: Vec<ServiceTick>,
}

impl ServicesCollector {
    pub fn new() -> Self {
        Self {
            last_sample_at: None,
            cached: Vec::new(),
        }
    }

    pub fn sample(&mut self) -> Vec<ServiceTick> {
        let stale = self
            .last_sample_at
            .map(|t| t.elapsed() >= REFRESH)
            .unwrap_or(true);
        if stale {
            self.cached = sample_inner();
            self.last_sample_at = Some(Instant::now());
        }
        self.cached.clone()
    }
}

#[cfg(target_os = "macos")]
fn sample_inner() -> Vec<ServiceTick> {
    use std::process::Command;
    let Ok(out) = Command::new("launchctl").arg("list").output() else {
        return Vec::new();
    };
    parse_launchctl_list(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(target_os = "linux")]
fn sample_inner() -> Vec<ServiceTick> {
    use std::process::Command;
    let Ok(out) = Command::new("systemctl")
        .args([
            "list-units",
            "--type=service",
            "--all",
            "--no-legend",
            "--plain",
            "--no-pager",
        ])
        .output()
    else {
        return Vec::new();
    };
    parse_systemctl_list(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn sample_inner() -> Vec<ServiceTick> {
    Vec::new()
}

pub fn parse_launchctl_list(text: &str) -> Vec<ServiceTick> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if i == 0 && line.starts_with("PID") {
            continue;
        }
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() != 3 {
            continue;
        }
        let pid_str = parts[0].trim();
        let status_str = parts[1].trim();
        let label = parts[2].trim();
        if label.is_empty() {
            continue;
        }
        let pid = pid_str.parse::<u32>().ok();
        let exit_code = status_str.parse::<i32>().ok();
        let status = match (pid, exit_code) {
            (Some(_), _) => ServiceStatus::Running,
            // No PID + non-zero exit = a service that ran and didn't end cleanly.
            (None, Some(c)) if c != 0 => ServiceStatus::Failed,
            (None, Some(_)) => ServiceStatus::Idle,
            (None, None) => ServiceStatus::Unknown,
        };
        let detail = match exit_code {
            Some(c) if c < 0 => format!("killed by signal {}", -c),
            Some(c) if c > 0 => format!("exit code {}", c),
            Some(_) => "clean exit / never run".into(),
            None => "no exit status reported".into(),
        };
        out.push(ServiceTick {
            name: label.to_string(),
            status,
            pid,
            exit_code,
            detail,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[cfg(any(target_os = "linux", test))]
pub fn parse_systemctl_list(text: &str) -> Vec<ServiceTick> {
    let mut out = Vec::new();
    for line in text.lines() {
        // UNIT LOAD ACTIVE SUB DESCRIPTION (description may contain spaces).
        let mut iter = line.split_whitespace();
        let Some(unit) = iter.next() else { continue };
        let Some(_load) = iter.next() else { continue };
        let Some(active) = iter.next() else { continue };
        let Some(sub) = iter.next() else { continue };
        let description: String = iter.collect::<Vec<_>>().join(" ");

        let status = match active {
            "active" if sub == "running" => ServiceStatus::Running,
            "active" => ServiceStatus::Idle, // exited / waiting
            "failed" => ServiceStatus::Failed,
            "inactive" => ServiceStatus::Idle,
            _ => ServiceStatus::Unknown,
        };
        out.push(ServiceTick {
            name: unit.to_string(),
            status,
            pid: None,
            exit_code: None,
            detail: format!("{} / {} — {}", active, sub, description),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_launchctl_real_sample() {
        // Captured from the dev machine.
        let sample = "\
PID\tStatus\tLabel
-\t0\tcom.apple.SafariHistoryServiceAgent
-\t-9\tcom.apple.progressd
83228\t-9\tcom.apple.bird
50304\t-9\tcom.apple.SafariBookmarksSyncAgent
-\t0\t
";
        let svcs = parse_launchctl_list(sample);
        // Empty label gets skipped.
        assert_eq!(svcs.len(), 4);

        let bird = svcs.iter().find(|s| s.name == "com.apple.bird").unwrap();
        assert_eq!(bird.pid, Some(83228));
        assert_eq!(bird.exit_code, Some(-9));
        assert_eq!(bird.status, ServiceStatus::Running);
        assert!(bird.detail.contains("signal 9"));

        let progressd = svcs
            .iter()
            .find(|s| s.name == "com.apple.progressd")
            .unwrap();
        assert_eq!(progressd.pid, None);
        assert_eq!(progressd.exit_code, Some(-9));
        assert_eq!(progressd.status, ServiceStatus::Failed);

        let safari = svcs
            .iter()
            .find(|s| s.name == "com.apple.SafariHistoryServiceAgent")
            .unwrap();
        assert_eq!(safari.status, ServiceStatus::Idle);
    }

    #[test]
    fn parses_systemctl_sample() {
        let sample = "\
sshd.service          loaded active running OpenSSH server daemon
nginx.service         loaded failed failed  nginx web server
cron.service          loaded active exited  Periodic command scheduler
foo.service           loaded inactive dead   Foo
";
        let svcs = parse_systemctl_list(sample);
        assert_eq!(svcs.len(), 4);
        assert_eq!(
            svcs.iter()
                .find(|s| s.name == "sshd.service")
                .unwrap()
                .status,
            ServiceStatus::Running
        );
        assert_eq!(
            svcs.iter()
                .find(|s| s.name == "nginx.service")
                .unwrap()
                .status,
            ServiceStatus::Failed
        );
        assert_eq!(
            svcs.iter()
                .find(|s| s.name == "cron.service")
                .unwrap()
                .status,
            ServiceStatus::Idle
        );
    }
}
