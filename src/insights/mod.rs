//! Heuristic anomaly detection over the rolling session.
//!
//! Pure functions: each `insight_*` reads `(History, &Snapshot)` and returns
//! `Option<Insight>`. `compute()` runs them all and sorts by severity.
//!
//! Read-only by design — Insights surface what to look at, never mutate.

use crate::app::{History, TabId};
use crate::collect::Snapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warn,
    Crit,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Info => "INFO",
            Severity::Warn => "WARN",
            Severity::Crit => "CRIT",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Insight {
    pub severity: Severity,
    pub title: String,
    pub body: Vec<String>,
    pub suggested_tab: TabId,
}

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * 1024 * 1024;

pub fn compute(h: &History, snap: &Snapshot) -> Vec<Insight> {
    let mut out: Vec<Insight> = Vec::new();
    if let Some(i) = insight_swap_thrash(h, snap) {
        out.push(i);
    }
    if let Some(i) = insight_runaway_proc(h, snap) {
        out.push(i);
    }
    if let Some(i) = insight_disk_full(snap) {
        out.push(i);
    }
    if let Some(i) = insight_memory_pressure(h, snap) {
        out.push(i);
    }
    if let Some(i) = insight_high_load(snap) {
        out.push(i);
    }
    if let Some(i) = insight_zombie_party(snap) {
        out.push(i);
    }
    if let Some(i) = insight_gpu_pegged(h, snap) {
        out.push(i);
    }
    if let Some(i) = insight_vram_high(snap) {
        out.push(i);
    }
    if let Some(i) = insight_psi_memory(snap) {
        out.push(i);
    }
    if let Some(i) = insight_psi_io(snap) {
        out.push(i);
    }
    if let Some(i) = insight_energy_hog(h, snap) {
        out.push(i);
    }
    if let Some(i) = insight_mem_leak(h, snap) {
        out.push(i);
    }

    // Most severe first; cap to the spec's ~6 cards budget.
    out.sort_by(|a, b| b.severity.cmp(&a.severity));
    out.truncate(6);
    out
}

/// Swap usage growing meaningfully over the rolling window.
fn insight_swap_thrash(h: &History, snap: &Snapshot) -> Option<Insight> {
    let history = h.swap.to_vec();
    if history.len() < 8 {
        return None;
    }
    let now = *history.last().unwrap();
    if now < 64 * MIB {
        return None;
    }
    // Compare current to value ~30 ticks ago (or the oldest we have).
    let baseline_idx = history.len().saturating_sub(30);
    let baseline = history[baseline_idx];
    let growth = now.saturating_sub(baseline);

    let (severity, title) = if growth >= 512 * MIB {
        (
            Severity::Crit,
            format!(
                "swap thrash — {:.1} GB swapped, +{:.0} MB in last {}s",
                now as f64 / GIB as f64,
                growth as f64 / MIB as f64,
                history.len() - baseline_idx
            ),
        )
    } else if growth >= 100 * MIB {
        (
            Severity::Warn,
            format!(
                "memory pressure — swap rising +{:.0} MB over last {}s",
                growth as f64 / MIB as f64,
                history.len() - baseline_idx
            ),
        )
    } else {
        return None;
    };

    // Name the most useful culprit the data can support: the biggest
    // per-process swap holder when the sampler has it (the actual
    // thrash driver), else the largest honest memory holder
    // (footprint / PSS), else RSS as the last resort.
    let culprit = snap
        .procs
        .iter()
        .filter_map(|p| p.mem_swap.map(|s| (p, s)))
        .filter(|(_, s)| *s > 0)
        .max_by_key(|(_, s)| *s)
        .map(|(p, s)| format!("{} holds the most swap ({}).", p.name, fmt_bytes(s)))
        .or_else(|| {
            snap.procs
                .iter()
                .filter_map(|p| p.mem_footprint.or(p.mem_pss).map(|m| (p, m)))
                .max_by_key(|(_, m)| *m)
                .map(|(p, m)| format!("{} holds the largest footprint ({}).", p.name, fmt_bytes(m)))
        })
        .or_else(|| {
            snap.procs.iter().max_by_key(|p| p.mem_rss).map(|p| {
                format!(
                    "{} holds the largest resident set ({}).",
                    p.name,
                    fmt_bytes(p.mem_rss)
                )
            })
        })
        .unwrap_or_default();

    Some(Insight {
        severity,
        title,
        body: vec![
            format!(
                "swap is {} of {} configured.",
                fmt_bytes(snap.mem.swap_used_bytes),
                fmt_bytes(snap.mem.swap_total_bytes.max(1))
            ),
            culprit,
        ],
        suggested_tab: TabId::Memory,
    })
}

/// A single process whose CPU has been sustained high (EWMA over recent ticks).
fn insight_runaway_proc(h: &History, snap: &Snapshot) -> Option<Insight> {
    let mut top: Option<(u32, f32)> = None;
    for (pid, ewma) in &h.proc_cpu_ewma {
        if *ewma >= 50.0 {
            if top.map_or(true, |(_, v)| *ewma > v) {
                top = Some((*pid, *ewma));
            }
        }
    }
    let (pid, ewma) = top?;
    let proc_ = snap.procs.iter().find(|p| p.pid == pid)?;
    let severity = if ewma >= 90.0 {
        Severity::Crit
    } else {
        Severity::Warn
    };
    Some(Insight {
        severity,
        title: format!(
            "runaway process — {} (pid {}) sustained {:.0}% CPU",
            proc_.name, pid, ewma
        ),
        body: vec![
            format!(
                "instantaneous {:.1}% / RSS {} / state {}",
                proc_.cpu_pct,
                fmt_bytes(proc_.mem_rss),
                proc_.state
            ),
            format!("user {} / ppid {}", proc_.user, proc_.ppid),
        ],
        suggested_tab: TabId::Procs,
    })
}

/// Most-full mount above the warn/crit threshold.
fn insight_disk_full(snap: &Snapshot) -> Option<Insight> {
    // Skip read-only mounts: composefs/overlay roots on immutable distros
    // (Fedora Atomic, ostree), squashfs, iso9660, etc. report 100% full by
    // design and can never be cleared, so warning about them is noise (#9).
    let worst = snap
        .disks
        .iter()
        .filter(|d| d.total_bytes > 0 && !d.read_only)
        .max_by(|a, b| {
            a.usage_pct
                .partial_cmp(&b.usage_pct)
                .unwrap_or(std::cmp::Ordering::Equal)
        })?;
    let severity = if worst.usage_pct >= 95.0 {
        Severity::Crit
    } else if worst.usage_pct >= 85.0 {
        Severity::Warn
    } else {
        return None;
    };
    let warn_count = snap
        .disks
        .iter()
        .filter(|d| d.usage_pct >= 85.0 && !d.read_only)
        .count();
    Some(Insight {
        severity,
        title: format!(
            "{} is {:.1}% full ({} of {})",
            worst.mount_point,
            worst.usage_pct,
            fmt_bytes(worst.used_bytes),
            fmt_bytes(worst.total_bytes)
        ),
        body: vec![
            format!(
                "{} free / fs {}",
                fmt_bytes(worst.available_bytes),
                worst.fs_type
            ),
            if warn_count > 1 {
                format!("{} mounts are above 85% utilization.", warn_count)
            } else {
                "no other mounts above 85% utilization.".into()
            },
        ],
        suggested_tab: TabId::Fs,
    })
}

/// RAM utilization sustained high over the window.
fn insight_memory_pressure(h: &History, snap: &Snapshot) -> Option<Insight> {
    if snap.mem.total_bytes == 0 {
        return None;
    }
    let recent: Vec<f32> = h.mem.to_vec();
    let last_n = 6usize.min(recent.len());
    if last_n < 3 {
        return None;
    }
    let avg = recent[recent.len() - last_n..].iter().sum::<f32>() / last_n as f32;
    let severity = if avg >= 0.95 {
        Severity::Crit
    } else if avg >= 0.85 {
        Severity::Warn
    } else {
        return None;
    };
    Some(Insight {
        severity,
        title: format!(
            "memory pressure — RAM {:.0}% used over last {}s",
            avg * 100.0,
            last_n
        ),
        body: vec![
            format!(
                "{} of {} ({} available)",
                fmt_bytes(snap.mem.used_bytes),
                fmt_bytes(snap.mem.total_bytes),
                fmt_bytes(snap.mem.available_bytes)
            ),
            "Sustained high pressure typically precedes swap activity or OOM kills.".into(),
        ],
        suggested_tab: TabId::Memory,
    })
}

/// Load average meaningfully above core count.
fn insight_high_load(snap: &Snapshot) -> Option<Insight> {
    let cores = snap.cpu.per_core.len().max(1) as f32;
    let load = snap.cpu.load_1;
    if load < cores * 1.5 {
        return None;
    }
    let severity = if load >= cores * 4.0 {
        Severity::Crit
    } else if load >= cores * 2.0 {
        Severity::Warn
    } else {
        Severity::Info
    };
    if severity == Severity::Info {
        return None;
    }
    Some(Insight {
        severity,
        title: format!(
            "load {:.2} on {} cores ({:.1}× saturation)",
            load,
            cores as u32,
            load / cores
        ),
        body: vec![
            format!(
                "load 1m / 5m / 15m  =  {:.2} / {:.2} / {:.2}",
                snap.cpu.load_1, snap.cpu.load_5, snap.cpu.load_15
            ),
            "Sustained load above 2× cores indicates a queue forming on the run queue.".into(),
        ],
        suggested_tab: TabId::Cpu,
    })
}

/// More than a handful of zombie processes — a parent isn't reaping.
fn insight_zombie_party(snap: &Snapshot) -> Option<Insight> {
    let zombies: Vec<&crate::collect::ProcTick> =
        snap.procs.iter().filter(|p| p.state == 'Z').collect();
    if zombies.len() < 5 {
        return None;
    }
    let severity = if zombies.len() >= 25 {
        Severity::Crit
    } else {
        Severity::Warn
    };
    let parents: Vec<u32> = {
        let mut v: Vec<u32> = zombies.iter().map(|z| z.ppid).collect();
        v.sort_unstable();
        v.dedup();
        v.truncate(5);
        v
    };
    Some(Insight {
        severity,
        title: format!("{} zombie processes — parent isn't reaping", zombies.len()),
        body: vec![
            format!(
                "Common parent pids: {}",
                parents
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            "Restart the parent or send it SIGCHLD; zombies hold no resources but indicate a bug."
                .into(),
        ],
        suggested_tab: TabId::Procs,
    })
}

/// Aggregate GPU util sustained near the ceiling. Reads the rolling
/// `gpu_util` ring (max-across-devices per tick) and looks for an average
/// ≥ 90% over the recent window, mirroring the memory-pressure pattern.
/// Skipped silently when no device has ever reported util in the window
/// (avoids firing on Linux NVIDIA without nvml installed).
fn insight_gpu_pegged(h: &History, snap: &Snapshot) -> Option<Insight> {
    let recent: Vec<f32> = h.gpu_util.to_vec();
    let last_n = 6usize.min(recent.len());
    if last_n < 3 {
        return None;
    }
    let window = &recent[recent.len() - last_n..];
    // If every sample in the window is exactly 0, no GPU is reporting util
    // at all — don't fire on missing data.
    if window.iter().all(|v| *v == 0.0) {
        return None;
    }
    let avg = window.iter().sum::<f32>() / last_n as f32;
    let severity = if avg >= 98.0 {
        Severity::Crit
    } else if avg >= 90.0 {
        Severity::Warn
    } else {
        return None;
    };
    let busiest = snap
        .gpus
        .iter()
        .filter_map(|g| g.util_pct.map(|u| (g, u)))
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(g, u)| format!("{} (vendor {}) at {:.0}%", g.name, g.vendor, u))
        .unwrap_or_else(|| "no per-device util reported".into());
    Some(Insight {
        severity,
        title: format!(
            "GPU pegged — {:.0}% util sustained over last {}s",
            avg, last_n
        ),
        body: vec![
            busiest,
            "Sustained near-ceiling GPU load — check the renderer/tiler split or top GPU procs."
                .into(),
        ],
        suggested_tab: TabId::Gpu,
    })
}

/// Any GPU whose VRAM occupancy is dangerously close to its limit. Doesn't
/// need history — the moment used/total crosses the threshold the user wants
/// to know.
fn insight_vram_high(snap: &Snapshot) -> Option<Insight> {
    let worst = snap
        .gpus
        .iter()
        .filter_map(|g| match (g.vram_used_bytes, g.vram_total_bytes) {
            (Some(u), Some(t)) if t > 0 => Some((g, u, t, u as f32 / t as f32)),
            _ => None,
        })
        .max_by(|a, b| a.3.partial_cmp(&b.3).unwrap_or(std::cmp::Ordering::Equal))?;
    let (g, used, total, frac) = worst;
    let pct = frac * 100.0;
    let severity = if pct >= 95.0 {
        Severity::Crit
    } else if pct >= 85.0 {
        Severity::Warn
    } else {
        return None;
    };
    Some(Insight {
        severity,
        title: format!(
            "{} VRAM {:.0}% used ({} of {})",
            g.name,
            pct,
            fmt_bytes(used),
            fmt_bytes(total)
        ),
        body: vec![
            "VRAM exhaustion forces the driver to spill — texture thrash or OOM-kill of the GPU process is likely soon.".into(),
            "Close other GPU clients or reduce model/batch sizes.".into(),
        ],
        suggested_tab: TabId::Gpu,
    })
}

/// Kernel-reported memory stall time (PSI, Linux). `full` means every
/// non-idle task was stalled at once — reclaim/thrash is actively
/// costing throughput, regardless of what %used says.
fn insight_psi_memory(snap: &Snapshot) -> Option<Insight> {
    let psi = snap.pressure.as_ref()?;
    let severity = if psi.mem_full >= 5.0 {
        Severity::Crit
    } else if psi.mem_some >= 10.0 {
        Severity::Warn
    } else {
        return None;
    };
    Some(Insight {
        severity,
        title: format!(
            "memory stall — tasks stalled {:.1}% of time (full {:.1}%)",
            psi.mem_some, psi.mem_full
        ),
        body: vec![
            "PSI avg10 from /proc/pressure/memory — direct kernel accounting of time lost to reclaim.".into(),
            format!(
                "RAM {} of {} used / swap {} used.",
                fmt_bytes(snap.mem.used_bytes),
                fmt_bytes(snap.mem.total_bytes),
                fmt_bytes(snap.mem.swap_used_bytes)
            ),
        ],
        suggested_tab: TabId::Memory,
    })
}

/// Kernel-reported IO stall time (PSI, Linux) — catches a saturated
/// disk even when raw throughput looks modest (small random IO).
fn insight_psi_io(snap: &Snapshot) -> Option<Insight> {
    let psi = snap.pressure.as_ref()?;
    let severity = if psi.io_full >= 15.0 {
        Severity::Crit
    } else if psi.io_some >= 25.0 {
        Severity::Warn
    } else {
        return None;
    };
    // Name the heaviest IO process to make the card actionable.
    let top_io = snap
        .procs
        .iter()
        .max_by(|a, b| {
            (a.io_read_rate + a.io_write_rate)
                .partial_cmp(&(b.io_read_rate + b.io_write_rate))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .filter(|p| p.io_read_rate + p.io_write_rate > 0.0)
        .map(|p| {
            format!(
                "{} is the heaviest IO proc ({} read / {} write).",
                p.name,
                crate::ui::widgets::human_rate(p.io_read_rate),
                crate::ui::widgets::human_rate(p.io_write_rate)
            )
        })
        .unwrap_or_else(|| "no single process dominates the visible IO.".into());
    Some(Insight {
        severity,
        title: format!(
            "io stall — tasks blocked on disk {:.1}% of time (full {:.1}%)",
            psi.io_some, psi.io_full
        ),
        body: vec!["PSI avg10 from /proc/pressure/io.".into(), top_io],
        suggested_tab: TabId::Disks,
    })
}

/// A single process sustaining a heavy billed-power draw (macOS).
fn insight_energy_hog(h: &History, snap: &Snapshot) -> Option<Insight> {
    let (pid, ewma) = h
        .proc_power_ewma
        .iter()
        .map(|(pid, w)| (*pid, *w))
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))?;
    if ewma < 8.0 {
        return None;
    }
    let proc_ = snap.procs.iter().find(|p| p.pid == pid)?;
    let severity = if ewma >= 15.0 {
        Severity::Warn
    } else {
        Severity::Info
    };
    Some(Insight {
        severity,
        title: format!(
            "energy hog — {} (pid {}) drawing ~{:.1} W sustained",
            proc_.name, pid, ewma
        ),
        body: vec![
            format!(
                "instantaneous ~{:.1} W / cpu {:.1}%",
                proc_.power_w.unwrap_or(0.0),
                proc_.cpu_pct
            ),
            "Estimate: measured CPU-rail power (IOReport) split by CPU share.".into(),
        ],
        suggested_tab: TabId::Power,
    })
}

/// Leak heuristic: a process whose *honest* memory metric (footprint /
/// private — pages that are really its own) has grown substantially
/// and proportionally since we first saw it. RSS is deliberately not
/// used: shared-page noise makes it cry wolf.
fn insight_mem_leak(h: &History, snap: &Snapshot) -> Option<Insight> {
    // ~2 minutes of sightings before judging; both absolute and
    // relative growth required so neither big-but-stable nor
    // tiny-but-doubling procs trip it.
    const MIN_TICKS: u32 = 120;
    const MIN_GROWTH: u64 = 256 * MIB;
    let (pid, (base, _ticks, latest)) = h
        .proc_mem_track
        .iter()
        .filter(|(_, (base, ticks, latest))| {
            *ticks >= MIN_TICKS
                && latest.saturating_sub(*base) >= MIN_GROWTH
                && (*latest as f64) >= (*base as f64) * 1.3
        })
        .max_by_key(|(_, (base, _, latest))| latest.saturating_sub(*base))?;
    let proc_ = snap.procs.iter().find(|p| p.pid == *pid)?;
    let grown = latest.saturating_sub(*base);
    Some(Insight {
        severity: Severity::Warn,
        title: format!(
            "possible leak — {} (pid {}) grew {} this session",
            proc_.name,
            pid,
            fmt_bytes(grown)
        ),
        body: vec![
            format!(
                "{} → {} (private/footprint, shared pages excluded)",
                fmt_bytes(*base),
                fmt_bytes(*latest)
            ),
            "Sustained private-memory growth without plateau is the classic leak signature.".into(),
        ],
        suggested_tab: TabId::Memory,
    })
}

fn fmt_bytes(b: u64) -> String {
    crate::ui::widgets::human_bytes(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::{DiskUsageTick, GpuTick, MemTick, ProcTick, Snapshot};

    // ── Fixture builders ──────────────────────────────────────────────────

    fn proc(pid: u32, name: &str, cpu: f32, rss: u64, state: char) -> ProcTick {
        ProcTick {
            pid,
            name: name.into(),
            cpu_pct: cpu,
            mem_rss: rss,
            state,
            ..Default::default()
        }
    }

    fn snap(mem: MemTick, procs: Vec<ProcTick>, disks: Vec<DiskUsageTick>) -> Snapshot {
        Snapshot {
            mem,
            procs,
            disks,
            ..Default::default()
        }
    }

    fn mem(used: u64, total: u64, swap_used: u64, swap_total: u64) -> MemTick {
        MemTick {
            total_bytes: total,
            used_bytes: used,
            available_bytes: total.saturating_sub(used),
            swap_total_bytes: swap_total,
            swap_used_bytes: swap_used,
            ..Default::default()
        }
    }

    fn cpu(load_1: f32, cores: usize) -> crate::collect::CpuTick {
        crate::collect::CpuTick {
            load_1,
            load_5: load_1,
            load_15: load_1,
            usage_pct: 0.0,
            per_core: vec![0.0; cores],
        }
    }

    fn disk(mount: &str, used_pct: f32) -> DiskUsageTick {
        DiskUsageTick {
            mount_point: mount.into(),
            device: format!("/dev/{}", mount),
            fs_type: "ext4".into(),
            total_bytes: 100 * GIB,
            used_bytes: ((used_pct / 100.0) * 100.0 * GIB as f32) as u64,
            available_bytes: ((1.0 - used_pct / 100.0) * 100.0 * GIB as f32) as u64,
            usage_pct: used_pct,
            read_only: false,
        }
    }

    /// A read-only mount (composefs/overlay root, squashfs, …) at `used_pct`.
    fn disk_ro(mount: &str, fs_type: &str, used_pct: f32) -> DiskUsageTick {
        DiskUsageTick {
            fs_type: fs_type.into(),
            read_only: true,
            ..disk(mount, used_pct)
        }
    }

    fn empty_history() -> History {
        // Minimum cap that satisfies all heuristics' window checks.
        let mut h = History::new(60);
        let _ = &mut h;
        h
    }

    fn first_with(insights: &[Insight], needle: &str) -> Option<Insight> {
        insights.iter().find(|i| i.title.contains(needle)).cloned()
    }

    // ── swap_thrash ───────────────────────────────────────────────────────

    #[test]
    fn swap_thrash_does_not_fire_below_threshold() {
        let mut h = empty_history();
        // Push 30 ticks of stable swap usage at 200 MIB.
        for _ in 0..30 {
            h.push(&snap(mem(0, 16 * GIB, 200 * MIB, 4 * GIB), vec![], vec![]));
        }
        let s = snap(mem(0, 16 * GIB, 200 * MIB, 4 * GIB), vec![], vec![]);
        let result = compute(&h, &s);
        assert!(first_with(&result, "swap").is_none());
    }

    #[test]
    fn swap_thrash_fires_warn_on_growth() {
        let mut h = empty_history();
        // 30 ticks at 100 MiB, then jump to 350 MiB (+250 MiB, > warn).
        for _ in 0..30 {
            h.push(&snap(mem(0, 16 * GIB, 100 * MIB, 4 * GIB), vec![], vec![]));
        }
        let final_swap = 350 * MIB;
        h.push(&snap(mem(0, 16 * GIB, final_swap, 4 * GIB), vec![], vec![]));
        let s = snap(mem(0, 16 * GIB, final_swap, 4 * GIB), vec![], vec![]);
        let result = compute(&h, &s);
        let ins = first_with(&result, "swap").expect("swap insight expected");
        assert_eq!(ins.severity, Severity::Warn);
    }

    #[test]
    fn swap_thrash_fires_crit_on_huge_growth() {
        let mut h = empty_history();
        for _ in 0..30 {
            h.push(&snap(mem(0, 16 * GIB, 100 * MIB, 4 * GIB), vec![], vec![]));
        }
        let final_swap = 700 * MIB; // +600 MiB > crit threshold (512)
        h.push(&snap(mem(0, 16 * GIB, final_swap, 4 * GIB), vec![], vec![]));
        let s = snap(mem(0, 16 * GIB, final_swap, 4 * GIB), vec![], vec![]);
        let result = compute(&h, &s);
        let ins = first_with(&result, "swap").expect("swap insight expected");
        assert_eq!(ins.severity, Severity::Crit);
    }

    // ── runaway_proc (uses History::proc_cpu_ewma) ────────────────────────

    #[test]
    fn runaway_proc_does_not_fire_for_transient_spike() {
        let mut h = empty_history();
        // Quiet history then one spiking sample → EWMA still low.
        for _ in 0..10 {
            h.push(&snap(
                MemTick::default(),
                vec![proc(42, "calm", 5.0, 0, 'S')],
                vec![],
            ));
        }
        h.push(&snap(
            MemTick::default(),
            vec![proc(42, "calm", 95.0, 0, 'S')],
            vec![],
        ));
        let s = snap(
            MemTick::default(),
            vec![proc(42, "calm", 95.0, 0, 'S')],
            vec![],
        );
        let result = compute(&h, &s);
        assert!(first_with(&result, "runaway").is_none());
    }

    #[test]
    fn runaway_proc_fires_warn_when_sustained() {
        let mut h = empty_history();
        // Sustained 70% CPU pulls EWMA above the 50% warn line.
        for _ in 0..15 {
            h.push(&snap(
                MemTick::default(),
                vec![proc(42, "rustc", 70.0, 0, 'R')],
                vec![],
            ));
        }
        let s = snap(
            MemTick::default(),
            vec![proc(42, "rustc", 70.0, 0, 'R')],
            vec![],
        );
        let result = compute(&h, &s);
        let ins = first_with(&result, "runaway").expect("runaway insight expected");
        assert_eq!(ins.severity, Severity::Warn);
        assert!(ins.title.contains("rustc"));
    }

    #[test]
    fn runaway_proc_fires_crit_at_sustained_95pct() {
        let mut h = empty_history();
        for _ in 0..15 {
            h.push(&snap(
                MemTick::default(),
                vec![proc(42, "rustc", 99.0, 0, 'R')],
                vec![],
            ));
        }
        let s = snap(
            MemTick::default(),
            vec![proc(42, "rustc", 99.0, 0, 'R')],
            vec![],
        );
        let result = compute(&h, &s);
        let ins = first_with(&result, "runaway").expect("runaway insight expected");
        assert_eq!(ins.severity, Severity::Crit);
    }

    // ── disk_full ─────────────────────────────────────────────────────────

    #[test]
    fn disk_full_does_not_fire_below_85pct() {
        let h = empty_history();
        let s = snap(MemTick::default(), vec![], vec![disk("/", 80.0)]);
        let result = compute(&h, &s);
        assert!(first_with(&result, "full").is_none());
    }

    #[test]
    fn disk_full_fires_warn_at_85pct() {
        let h = empty_history();
        let s = snap(MemTick::default(), vec![], vec![disk("/", 88.0)]);
        let result = compute(&h, &s);
        let ins = first_with(&result, "full").expect("disk full insight expected");
        assert_eq!(ins.severity, Severity::Warn);
        assert_eq!(ins.suggested_tab, TabId::Fs);
    }

    #[test]
    fn disk_full_fires_crit_at_95pct() {
        let h = empty_history();
        let s = snap(MemTick::default(), vec![], vec![disk("/", 96.5)]);
        let result = compute(&h, &s);
        let ins = first_with(&result, "full").expect("disk full insight expected");
        assert_eq!(ins.severity, Severity::Crit);
    }

    #[test]
    fn disk_full_ignores_readonly_composefs_root() {
        // Fedora Atomic / ostree roots are read-only composefs (fstype
        // `overlay`) reported at 100% full — must not warn (issue #9).
        let h = empty_history();
        let s = snap(
            MemTick::default(),
            vec![],
            vec![disk_ro("/", "overlay", 100.0)],
        );
        let result = compute(&h, &s);
        assert!(
            first_with(&result, "full").is_none(),
            "read-only composefs root should not raise a disk-full insight"
        );
    }

    #[test]
    fn disk_full_picks_writable_mount_over_full_readonly_root() {
        // A full read-only root must be skipped in favor of the genuinely
        // actionable writable mount, not merely suppressed.
        let h = empty_history();
        let s = snap(
            MemTick::default(),
            vec![],
            vec![disk_ro("/", "overlay", 100.0), disk("/var", 91.0)],
        );
        let result = compute(&h, &s);
        let ins = first_with(&result, "full").expect("writable mount should warn");
        assert_eq!(ins.severity, Severity::Warn);
        assert!(
            ins.title.contains("/var"),
            "insight should target the writable mount, got: {}",
            ins.title
        );
    }

    // ── memory_pressure ──────────────────────────────────────────────────

    #[test]
    fn memory_pressure_does_not_fire_at_normal_load() {
        let mut h = empty_history();
        for _ in 0..10 {
            h.push(&snap(mem(8 * GIB, 16 * GIB, 0, 0), vec![], vec![]));
        }
        let s = snap(mem(8 * GIB, 16 * GIB, 0, 0), vec![], vec![]);
        let result = compute(&h, &s);
        assert!(first_with(&result, "memory pressure").is_none());
    }

    #[test]
    fn memory_pressure_fires_warn_at_85pct_sustained() {
        let mut h = empty_history();
        for _ in 0..10 {
            h.push(&snap(
                mem(15 * GIB - 200 * MIB, 16 * GIB, 0, 0),
                vec![],
                vec![],
            ));
        }
        let s = snap(mem(15 * GIB - 200 * MIB, 16 * GIB, 0, 0), vec![], vec![]);
        let result = compute(&h, &s);
        let ins = first_with(&result, "memory pressure").expect("memory pressure expected");
        assert_eq!(ins.severity, Severity::Warn);
    }

    #[test]
    fn memory_pressure_fires_crit_at_95pct() {
        let mut h = empty_history();
        for _ in 0..10 {
            h.push(&snap(mem(155 * GIB / 10, 16 * GIB, 0, 0), vec![], vec![]));
        }
        let s = snap(mem(155 * GIB / 10, 16 * GIB, 0, 0), vec![], vec![]);
        let result = compute(&h, &s);
        let ins = first_with(&result, "memory pressure").expect("memory pressure expected");
        assert_eq!(ins.severity, Severity::Crit);
    }

    // ── high_load ────────────────────────────────────────────────────────

    #[test]
    fn high_load_does_not_fire_under_warn_threshold() {
        let h = empty_history();
        let mut s = Snapshot::default();
        s.cpu = cpu(7.0, 8); // 7 < 8 * 1.5 = 12 → quiet
        let result = compute(&h, &s);
        assert!(first_with(&result, "load").is_none());
    }

    #[test]
    fn high_load_fires_warn_at_2x_cores() {
        let h = empty_history();
        let mut s = Snapshot::default();
        s.cpu = cpu(20.0, 8); // 20 > 8 * 2 = 16
        let result = compute(&h, &s);
        let ins = first_with(&result, "load").expect("high-load insight expected");
        assert_eq!(ins.severity, Severity::Warn);
    }

    #[test]
    fn high_load_fires_crit_at_4x_cores() {
        let h = empty_history();
        let mut s = Snapshot::default();
        s.cpu = cpu(40.0, 8); // 40 > 8 * 4 = 32
        let result = compute(&h, &s);
        let ins = first_with(&result, "load").expect("high-load insight expected");
        assert_eq!(ins.severity, Severity::Crit);
    }

    // ── zombie_party ──────────────────────────────────────────────────────

    #[test]
    fn zombie_party_does_not_fire_below_5() {
        let h = empty_history();
        let procs = (0..4).map(|i| proc(i, "z", 0.0, 0, 'Z')).collect();
        let s = snap(MemTick::default(), procs, vec![]);
        let result = compute(&h, &s);
        assert!(first_with(&result, "zombie").is_none());
    }

    #[test]
    fn zombie_party_fires_warn_at_5_zombies() {
        let h = empty_history();
        let procs = (0..7).map(|i| proc(i, "z", 0.0, 0, 'Z')).collect();
        let s = snap(MemTick::default(), procs, vec![]);
        let result = compute(&h, &s);
        let ins = first_with(&result, "zombie").expect("zombie insight expected");
        assert_eq!(ins.severity, Severity::Warn);
    }

    #[test]
    fn zombie_party_fires_crit_at_25_plus() {
        let h = empty_history();
        let procs = (0..30).map(|i| proc(i, "z", 0.0, 0, 'Z')).collect();
        let s = snap(MemTick::default(), procs, vec![]);
        let result = compute(&h, &s);
        let ins = first_with(&result, "zombie").expect("zombie insight expected");
        assert_eq!(ins.severity, Severity::Crit);
    }

    // ── gpu_pegged ───────────────────────────────────────────────────────

    fn snap_with_gpu(
        util: Option<f32>,
        vram_used: Option<u64>,
        vram_total: Option<u64>,
    ) -> Snapshot {
        Snapshot {
            gpus: vec![GpuTick {
                name: "TestGPU".into(),
                vendor: "TestCorp".into(),
                util_pct: util,
                vram_used_bytes: vram_used,
                vram_total_bytes: vram_total,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn gpu_pegged_does_not_fire_for_idle_gpu() {
        let mut h = empty_history();
        for _ in 0..10 {
            h.push(&snap_with_gpu(Some(15.0), None, None));
        }
        let s = snap_with_gpu(Some(15.0), None, None);
        let result = compute(&h, &s);
        assert!(first_with(&result, "GPU pegged").is_none());
    }

    #[test]
    fn gpu_pegged_does_not_fire_when_no_util_reported() {
        // Linux NVIDIA-without-nvml shape: util_pct = None on every tick →
        // gpu_util ring is all-zero → don't fire.
        let mut h = empty_history();
        for _ in 0..10 {
            h.push(&snap_with_gpu(None, None, None));
        }
        let s = snap_with_gpu(None, None, None);
        let result = compute(&h, &s);
        assert!(first_with(&result, "GPU pegged").is_none());
    }

    #[test]
    fn gpu_pegged_fires_warn_at_sustained_90pct() {
        let mut h = empty_history();
        for _ in 0..10 {
            h.push(&snap_with_gpu(Some(92.0), None, None));
        }
        let s = snap_with_gpu(Some(92.0), None, None);
        let result = compute(&h, &s);
        let ins = first_with(&result, "GPU pegged").expect("gpu pegged expected");
        assert_eq!(ins.severity, Severity::Warn);
        assert_eq!(ins.suggested_tab, TabId::Gpu);
    }

    #[test]
    fn gpu_pegged_fires_crit_at_sustained_98pct() {
        let mut h = empty_history();
        for _ in 0..10 {
            h.push(&snap_with_gpu(Some(99.0), None, None));
        }
        let s = snap_with_gpu(Some(99.0), None, None);
        let result = compute(&h, &s);
        let ins = first_with(&result, "GPU pegged").expect("gpu pegged expected");
        assert_eq!(ins.severity, Severity::Crit);
    }

    #[test]
    fn gpu_pegged_does_not_fire_for_single_spike() {
        let mut h = empty_history();
        for _ in 0..10 {
            h.push(&snap_with_gpu(Some(20.0), None, None));
        }
        // One tick at 100% — average over last 6 stays well below 90%.
        h.push(&snap_with_gpu(Some(100.0), None, None));
        let s = snap_with_gpu(Some(100.0), None, None);
        let result = compute(&h, &s);
        assert!(first_with(&result, "GPU pegged").is_none());
    }

    // ── vram_high ────────────────────────────────────────────────────────

    #[test]
    fn vram_high_does_not_fire_below_85pct() {
        let h = empty_history();
        let s = snap_with_gpu(Some(50.0), Some(8 * GIB), Some(16 * GIB)); // 50%
        assert!(first_with(&compute(&h, &s), "VRAM").is_none());
    }

    #[test]
    fn vram_high_fires_warn_at_85pct() {
        let h = empty_history();
        let s = snap_with_gpu(Some(50.0), Some(14 * GIB), Some(16 * GIB)); // 87.5%
        let ins = first_with(&compute(&h, &s), "VRAM").expect("vram insight expected");
        assert_eq!(ins.severity, Severity::Warn);
    }

    #[test]
    fn vram_high_fires_crit_at_95pct() {
        let h = empty_history();
        let s = snap_with_gpu(Some(50.0), Some(16 * GIB - 200 * MIB), Some(16 * GIB)); // ~98.8%
        let ins = first_with(&compute(&h, &s), "VRAM").expect("vram insight expected");
        assert_eq!(ins.severity, Severity::Crit);
    }

    #[test]
    fn vram_high_skips_devices_without_total() {
        let h = empty_history();
        // VRAM total unknown — can't compute fraction, must not fire.
        let s = snap_with_gpu(Some(50.0), Some(8 * GIB), None);
        assert!(first_with(&compute(&h, &s), "VRAM").is_none());
    }

    // ── compute() ordering / capping ──────────────────────────────────────

    #[test]
    fn compute_sorts_crit_before_warn() {
        let mut h = empty_history();
        // Build a state that triggers WARN swap_thrash AND CRIT disk_full.
        for _ in 0..30 {
            h.push(&snap(mem(0, 16 * GIB, 100 * MIB, 4 * GIB), vec![], vec![]));
        }
        let final_swap = 350 * MIB;
        h.push(&snap(mem(0, 16 * GIB, final_swap, 4 * GIB), vec![], vec![]));
        let s = snap(
            mem(0, 16 * GIB, final_swap, 4 * GIB),
            vec![],
            vec![disk("/", 96.0)],
        );
        let result = compute(&h, &s);
        assert!(result.len() >= 2);
        assert_eq!(result[0].severity, Severity::Crit);
        // Subsequent items are <= the first.
        for w in result.windows(2) {
            assert!(w[0].severity >= w[1].severity);
        }
    }

    #[test]
    fn compute_caps_at_six_cards() {
        // Construct a Snapshot that lights up every heuristic.
        let mut h = empty_history();
        for _ in 0..30 {
            h.push(&snap(
                mem(15 * GIB, 16 * GIB, 100 * MIB, 4 * GIB),
                vec![proc(42, "rustc", 95.0, 0, 'R')],
                vec![],
            ));
        }
        let final_swap = 700 * MIB;
        let zombies: Vec<ProcTick> = (0..30).map(|i| proc(100 + i, "z", 0.0, 0, 'Z')).collect();
        let mut all_procs = vec![proc(42, "rustc", 95.0, 0, 'R')];
        all_procs.extend(zombies);
        h.push(&snap(
            mem(15 * GIB, 16 * GIB, final_swap, 4 * GIB),
            all_procs.clone(),
            vec![],
        ));
        let mut s = snap(
            mem(15 * GIB, 16 * GIB, final_swap, 4 * GIB),
            all_procs,
            vec![disk("/", 96.0), disk("/data", 99.0)],
        );
        s.cpu = cpu(40.0, 8);
        let result = compute(&h, &s);
        assert!(
            result.len() <= 6,
            "should cap at 6 cards, got {}",
            result.len()
        );
    }
}
