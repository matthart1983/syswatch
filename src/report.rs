//! Non-interactive reporting: `snapshot`, `insights`, `diff`, `why`.
//!
//! Each prints a result to stdout and exits -- no TUI, no raw mode,
//! script-friendly. `--json` on the ones that support it serializes
//! the same `Snapshot`/`Insight` types the interactive TUI itself
//! uses, so what a script sees is exactly what the live tabs show,
//! not a separate reporting-only representation that could drift
//! from it.
//!
//! `insights` and `why` need a short window of live history -- a
//! single sample can't tell "sustained" from "momentary," which is
//! most of what the heuristics in `insights::compute` are built to
//! catch. There's no persistent background collector to query
//! instantly (see the README's anti-goals), so both block for
//! `--since` sampling on the spot, exactly as the interactive TUI
//! would fill its own history, before printing anything.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::app::History;
use crate::collect::{Collector, Snapshot};
use crate::insights::{self, Insight};
use crate::recording;
use crate::ui::widgets::human_bytes;

/// Sample the live host for `window` at `tick`, building a `History`
/// exactly as the interactive TUI would, and return it alongside the
/// last sample taken. Blocks for the full window.
fn sample_window(window: Duration, tick: Duration) -> (History, Snapshot) {
    let ticks = ((window.as_secs_f64() / tick.as_secs_f64()).ceil() as usize).max(1);
    eprintln!(
        "syswatch: sampling for {:.0}s ({} ticks at {}ms)...",
        window.as_secs_f64(),
        ticks,
        tick.as_millis()
    );
    let mut collector = Collector::new(tick.as_millis() as u64);
    // Always at least large enough to hold every tick this window
    // takes -- the insight heuristics look back by a fixed number of
    // ticks (or, for the leak detector, real elapsed time), not by a
    // fraction of history capacity, so nothing should evict here.
    let mut history = History::new(ticks.max(8));
    let mut last = collector.sample();
    history.push(&last);
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        std::thread::sleep(tick.min(deadline.saturating_duration_since(Instant::now())));
        if Instant::now() >= deadline {
            break;
        }
        last = collector.sample();
        history.push(&last);
    }
    (history, last)
}

// ── snapshot ─────────────────────────────────────────────────────────────

pub fn run_snapshot(json: bool) -> Result<()> {
    let mut collector = Collector::new(1000);
    let snap = collector.sample();
    if json {
        println!("{}", serde_json::to_string_pretty(&snap)?);
    } else {
        print_snapshot_text(&snap);
    }
    Ok(())
}

fn print_snapshot_text(snap: &Snapshot) {
    println!("host       {} ({})", snap.host.hostname, snap.host.os);
    println!(
        "cpu        {:.1}% across {} cores, load {:.2}",
        snap.cpu.usage_pct, snap.host.cpu_cores, snap.cpu.load_1
    );
    println!(
        "memory     {} / {} used ({} available)",
        human_bytes(snap.mem.used_bytes),
        human_bytes(snap.mem.total_bytes),
        human_bytes(snap.mem.available_bytes)
    );
    if snap.mem.swap_total_bytes > 0 {
        println!(
            "swap       {} / {} used",
            human_bytes(snap.mem.swap_used_bytes),
            human_bytes(snap.mem.swap_total_bytes)
        );
    }
    for d in &snap.disks {
        if d.total_bytes > 0 {
            println!(
                "disk {:<8} {:.0}% full ({} of {})",
                d.mount_point,
                d.usage_pct,
                human_bytes(d.used_bytes),
                human_bytes(d.total_bytes)
            );
        }
    }
    println!(
        "processes  {} ({} services)",
        snap.procs.len(),
        snap.services.len()
    );
    if let Some(p) = snap.pressure {
        println!(
            "pressure   mem {:.1}% / io {:.1}% / cpu {:.1}% (avg10, some)",
            p.mem_some, p.io_some, p.cpu_some
        );
    }
}

// ── insights ─────────────────────────────────────────────────────────────

pub fn run_insights(json: bool, since: Duration, tick_ms: u64) -> Result<()> {
    let tick = Duration::from_millis(tick_ms.clamp(100, 5000));
    let (history, last) = sample_window(since, tick);
    let cards = insights::compute(&history, &last);
    if json {
        println!("{}", serde_json::to_string_pretty(&cards)?);
    } else {
        print_insights_text(&cards);
    }
    Ok(())
}

fn print_insights_text(cards: &[Insight]) {
    if cards.is_empty() {
        println!("no insights fired in this window.");
        return;
    }
    for c in cards {
        println!("[{}] {}", c.severity.label(), c.title);
        for line in &c.body {
            println!("    {line}");
        }
        println!("    (see: {:?})", c.suggested_tab);
    }
}

// ── why ──────────────────────────────────────────────────────────────────

pub fn run_why(since: Duration, tick_ms: u64) -> Result<()> {
    let tick = Duration::from_millis(tick_ms.clamp(100, 5000));
    let (history, last) = sample_window(since, tick);
    let cards = insights::compute(&history, &last);
    if cards.is_empty() {
        println!(
            "Nothing worth flagging in the last {:.0}s -- the machine looks normal.",
            since.as_secs_f64()
        );
        return Ok(());
    }
    let plural = if cards.len() == 1 { "thing" } else { "things" };
    println!(
        "{} {plural} worth a look, from the last {:.0}s:\n",
        cards.len(),
        since.as_secs_f64()
    );
    for c in &cards {
        println!("- {} ({})", c.title, c.severity.label());
        for line in &c.body {
            println!("  {line}");
        }
        println!();
    }
    Ok(())
}

// ── diff ─────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct DiffReport {
    pub from_label: String,
    pub to_label: String,
    pub from_t: DateTime<Utc>,
    pub to_t: DateTime<Utc>,
    pub cpu_pct: (f32, f32),
    pub load_1: (f32, f32),
    pub mem_used_pct: (f32, f32),
    pub swap_used_bytes: (u64, u64),
    pub process_count: (usize, usize),
    pub service_count: (usize, usize),
    /// Disk usage pct for mounts present (by mount point) in both
    /// snapshots. A mount that only exists on one side (unmounted
    /// between captures, or a different host) is left out rather than
    /// diffed against nothing.
    pub disk_usage_pct: Vec<(String, f32, f32)>,
    /// Process names newly present in `to`, capped and sorted by name
    /// for a stable, readable order.
    pub new_processes: Vec<String>,
    /// Process names present in `from` but gone by `to`.
    pub exited_processes: Vec<String>,
    /// The biggest RSS growers among pids present in both snapshots,
    /// sorted by growth descending. A negative delta means the
    /// process shrank -- included because "grew" and "shrank the
    /// most" are both diagnostic, not just growth.
    pub top_rss_change: Vec<(String, i64)>,
}

const DIFF_LIST_CAP: usize = 10;

impl DiffReport {
    fn compute(from: &Snapshot, to: &Snapshot, from_label: String, to_label: String) -> Self {
        let mem_pct = |s: &Snapshot| {
            if s.mem.total_bytes == 0 {
                0.0
            } else {
                s.mem.used_bytes as f32 / s.mem.total_bytes as f32 * 100.0
            }
        };

        let mut disk_usage_pct = Vec::new();
        for d in &from.disks {
            if let Some(d2) = to.disks.iter().find(|d2| d2.mount_point == d.mount_point) {
                disk_usage_pct.push((d.mount_point.clone(), d.usage_pct, d2.usage_pct));
            }
        }
        disk_usage_pct.sort_by(|a, b| a.0.cmp(&b.0));

        let from_pids: std::collections::HashMap<u32, &crate::collect::ProcTick> =
            from.procs.iter().map(|p| (p.pid, p)).collect();
        let to_pids: std::collections::HashMap<u32, &crate::collect::ProcTick> =
            to.procs.iter().map(|p| (p.pid, p)).collect();

        let mut new_processes: Vec<String> = to_pids
            .iter()
            .filter(|(pid, _)| !from_pids.contains_key(pid))
            .map(|(_, p)| p.name.clone())
            .collect();
        new_processes.sort();
        new_processes.dedup();
        new_processes.truncate(DIFF_LIST_CAP);

        let mut exited_processes: Vec<String> = from_pids
            .iter()
            .filter(|(pid, _)| !to_pids.contains_key(pid))
            .map(|(_, p)| p.name.clone())
            .collect();
        exited_processes.sort();
        exited_processes.dedup();
        exited_processes.truncate(DIFF_LIST_CAP);

        let mut top_rss_change: Vec<(String, i64)> = from_pids
            .iter()
            .filter_map(|(pid, p_from)| {
                to_pids.get(pid).map(|p_to| {
                    (
                        p_to.name.clone(),
                        p_to.mem_rss as i64 - p_from.mem_rss as i64,
                    )
                })
            })
            .collect();
        top_rss_change.sort_by_key(|(_, delta)| std::cmp::Reverse(delta.abs()));
        top_rss_change.truncate(DIFF_LIST_CAP);

        Self {
            from_label,
            to_label,
            from_t: from.t.into(),
            to_t: to.t.into(),
            cpu_pct: (from.cpu.usage_pct, to.cpu.usage_pct),
            load_1: (from.cpu.load_1, to.cpu.load_1),
            mem_used_pct: (mem_pct(from), mem_pct(to)),
            swap_used_bytes: (from.mem.swap_used_bytes, to.mem.swap_used_bytes),
            process_count: (from.procs.len(), to.procs.len()),
            service_count: (from.services.len(), to.services.len()),
            disk_usage_pct,
            new_processes,
            exited_processes,
            top_rss_change,
        }
    }

    fn print_text(&self) {
        println!("from: {} ({})", self.from_label, self.from_t.to_rfc3339());
        println!("to:   {} ({})\n", self.to_label, self.to_t.to_rfc3339());
        println!(
            "cpu        {:.1}% -> {:.1}%",
            self.cpu_pct.0, self.cpu_pct.1
        );
        println!("load avg   {:.2} -> {:.2}", self.load_1.0, self.load_1.1);
        println!(
            "memory     {:.1}% -> {:.1}% used",
            self.mem_used_pct.0, self.mem_used_pct.1
        );
        if self.swap_used_bytes.0 > 0 || self.swap_used_bytes.1 > 0 {
            println!(
                "swap       {} -> {}",
                human_bytes(self.swap_used_bytes.0),
                human_bytes(self.swap_used_bytes.1)
            );
        }
        for (mount, from_pct, to_pct) in &self.disk_usage_pct {
            if (to_pct - from_pct).abs() > 0.05 {
                println!("disk {mount:<8} {from_pct:.1}% -> {to_pct:.1}%");
            }
        }
        println!(
            "processes  {} -> {}",
            self.process_count.0, self.process_count.1
        );
        println!(
            "services   {} -> {}",
            self.service_count.0, self.service_count.1
        );
        if !self.new_processes.is_empty() {
            println!("\nnew processes: {}", self.new_processes.join(", "));
        }
        if !self.exited_processes.is_empty() {
            println!("exited processes: {}", self.exited_processes.join(", "));
        }
        if !self.top_rss_change.is_empty() {
            println!("\nbiggest memory changes:");
            for (name, delta) in &self.top_rss_change {
                let sign = if *delta >= 0 { "+" } else { "-" };
                println!("  {name:<24} {sign}{}", human_bytes(delta.unsigned_abs()));
            }
        }
    }
}

pub fn run_diff(a: &Path, b: Option<&Path>, json: bool) -> Result<()> {
    let snaps_a = recording::read(a).with_context(|| format!("reading {}", a.display()))?;
    if snaps_a.is_empty() {
        bail!("{} contains no snapshots", a.display());
    }

    let report = match b {
        Some(b_path) => {
            let snaps_b =
                recording::read(b_path).with_context(|| format!("reading {}", b_path.display()))?;
            if snaps_b.is_empty() {
                bail!("{} contains no snapshots", b_path.display());
            }
            DiffReport::compute(
                snaps_a.last().unwrap(),
                snaps_b.last().unwrap(),
                format!("{} (last of {})", a.display(), snaps_a.len()),
                format!("{} (last of {})", b_path.display(), snaps_b.len()),
            )
        }
        None => {
            if snaps_a.len() < 2 {
                bail!(
                    "{} has only one snapshot -- nothing to diff. Pass a second file to compare two recordings.",
                    a.display()
                );
            }
            DiffReport::compute(
                snaps_a.first().unwrap(),
                snaps_a.last().unwrap(),
                format!("{} (first)", a.display()),
                format!("{} (last of {})", a.display(), snaps_a.len()),
            )
        }
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        report.print_text();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::ProcTick;
    use std::time::{Duration, SystemTime};

    fn snap_with_procs(t_secs: u64, procs: Vec<ProcTick>) -> Snapshot {
        Snapshot {
            t: SystemTime::UNIX_EPOCH + Duration::from_secs(t_secs),
            procs,
            ..Default::default()
        }
    }

    fn proc(pid: u32, name: &str, rss: u64) -> ProcTick {
        ProcTick {
            pid,
            name: name.into(),
            mem_rss: rss,
            ..Default::default()
        }
    }

    #[test]
    fn diff_detects_new_and_exited_processes() {
        let from = snap_with_procs(0, vec![proc(1, "stays", 100), proc(2, "leaves", 100)]);
        let to = snap_with_procs(10, vec![proc(1, "stays", 100), proc(3, "arrives", 100)]);
        let report = DiffReport::compute(&from, &to, "a".into(), "b".into());
        assert_eq!(report.new_processes, vec!["arrives".to_string()]);
        assert_eq!(report.exited_processes, vec!["leaves".to_string()]);
    }

    #[test]
    fn diff_reports_rss_growth_for_shared_pids_only() {
        let from = snap_with_procs(0, vec![proc(1, "grower", 100), proc(2, "gone", 50)]);
        let to = snap_with_procs(10, vec![proc(1, "grower", 300), proc(3, "new", 20)]);
        let report = DiffReport::compute(&from, &to, "a".into(), "b".into());
        assert_eq!(report.top_rss_change, vec![("grower".to_string(), 200)]);
    }

    #[test]
    fn diff_only_compares_disks_present_on_both_sides() {
        let mut from = snap_with_procs(0, vec![]);
        from.disks = vec![crate::collect::DiskUsageTick {
            mount_point: "/".into(),
            total_bytes: 100,
            usage_pct: 50.0,
            ..Default::default()
        }];
        let mut to = snap_with_procs(10, vec![]);
        to.disks = vec![
            crate::collect::DiskUsageTick {
                mount_point: "/".into(),
                total_bytes: 100,
                usage_pct: 70.0,
                ..Default::default()
            },
            crate::collect::DiskUsageTick {
                mount_point: "/data".into(),
                total_bytes: 100,
                usage_pct: 10.0,
                ..Default::default()
            },
        ];
        let report = DiffReport::compute(&from, &to, "a".into(), "b".into());
        assert_eq!(report.disk_usage_pct, vec![("/".to_string(), 50.0, 70.0)]);
    }

    #[test]
    fn diff_cpu_and_mem_deltas() {
        let mut from = snap_with_procs(0, vec![]);
        from.cpu.usage_pct = 10.0;
        from.mem.total_bytes = 1000;
        from.mem.used_bytes = 100;
        let mut to = snap_with_procs(10, vec![]);
        to.cpu.usage_pct = 90.0;
        to.mem.total_bytes = 1000;
        to.mem.used_bytes = 900;
        let report = DiffReport::compute(&from, &to, "a".into(), "b".into());
        assert_eq!(report.cpu_pct, (10.0, 90.0));
        assert_eq!(report.mem_used_pct, (10.0, 90.0));
    }

    #[test]
    fn run_snapshot_json_produces_valid_json() {
        // Smoke test against a real live sample -- confirms Snapshot's
        // Serialize impl round-trips through serde_json without
        // panicking on any real field value (NaN, etc. would fail
        // here if they ever appeared).
        let mut collector = Collector::new(1000);
        let snap = collector.sample();
        let json = serde_json::to_string(&snap).expect("Snapshot must serialize to JSON");
        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("must parse back as valid JSON");
        assert!(parsed.is_object());
    }

    #[test]
    fn run_insights_json_produces_valid_json_for_an_empty_history() {
        let history = History::new(10);
        let snap = Snapshot::default();
        let cards = insights::compute(&history, &snap);
        let json = serde_json::to_string(&cards).expect("Vec<Insight> must serialize to JSON");
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed.is_array());
    }
}
