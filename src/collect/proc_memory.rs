//! Per-process memory detail attribution.
//!
//! RSS double-counts shared pages — every process mapping a shared
//! library is charged its full size, so summing RSS overshoots real
//! RAM use badly on hosts with many forks of one binary. This
//! collector samples the platform's honest per-process accounting:
//!
//! * **Linux** — `/proc/{pid}/smaps_rollup`: PSS (shared pages divided
//!   by their mapper count, so PSS sums ≈ real footprint), the
//!   private/shared split, and per-process swap. Readable for the
//!   caller's own processes without privileges; other users' procs
//!   need root/ptrace and degrade to `None`.
//! * **macOS** — `proc_pid_rusage(RUSAGE_INFO_V2)`'s `ri_phys_footprint`,
//!   the same number Activity Monitor's Memory column shows. macOS RSS
//!   misleads twice over (compressor + shared frameworks); footprint is
//!   Apple's own per-process pressure accounting. Same-user procs only
//!   without sudo; others degrade to `None`.
//!
//! Costs: `smaps_rollup` makes the kernel walk the process's VMA list
//! (~0.1–2 ms each), so we cap to the top [`MAX_PROCS`] by RSS and
//! re-sample at most every [`REFRESH`] — bounded at a few tens of ms
//! every couple seconds, in line with the 1.5 s sysinfo process scan.
//! `proc_pid_rusage` is a single cheap syscall per PID.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::model::ProcTick;

const REFRESH: Duration = Duration::from_secs(3);
/// Detail is for the "what's eating my RAM" question — the top of the
/// RSS ranking answers it; walking VMAs for every idle daemon doesn't.
// The Memory tab renders at most `panel_height - 1` rows, well under
// this on any normal terminal (this project's own demos are recorded
// at 130x44). Leak tracking (`proc_mem_track`) keeps an entry once a
// pid enters the top-N, so this only delays first detection for a
// borderline grower rather than dropping coverage.
const MAX_PROCS: usize = 48;

#[derive(Debug, Clone, Copy, Default)]
pub struct ProcMem {
    /// macOS `ri_phys_footprint`. None elsewhere or on EPERM.
    pub footprint: Option<u64>,
    /// Linux Pss. None elsewhere or when smaps_rollup is unreadable.
    pub pss: Option<u64>,
    /// Linux Private_Clean + Private_Dirty — what actually frees if
    /// the process exits, and the honest leak signal.
    pub private: Option<u64>,
    /// Linux Shared_Clean + Shared_Dirty.
    pub shared: Option<u64>,
    /// Linux Swap — a process can sit on huge swap with a small RSS.
    pub swap: Option<u64>,
    /// macOS `ri_lifetime_max_phys_footprint` — lifetime peak.
    pub peak: Option<u64>,
}

impl ProcMem {
    fn is_empty(&self) -> bool {
        self.footprint.is_none() && self.pss.is_none()
    }
}

pub struct ProcMemCollector {
    last_sample_at: Option<Instant>,
    cached: HashMap<u32, ProcMem>,
    /// RSS seen for each pid at its last `smaps_rollup` read. A process
    /// whose RSS has not moved since is almost certainly unchanged in
    /// PSS/private/shared too, so its cached detail is reused instead
    /// of paying for another page-table walk. On an idle desktop that
    /// is most of the top-N, most of the time.
    last_rss: HashMap<u32, u64>,
}

impl ProcMemCollector {
    pub fn new() -> Self {
        Self {
            last_sample_at: None,
            cached: HashMap::new(),
            last_rss: HashMap::new(),
        }
    }

    /// Per-PID memory detail for the top procs by RSS. Re-samples at
    /// most every `REFRESH`; between samples returns the cached map.
    pub fn sample(&mut self, procs: &[ProcTick]) -> HashMap<u32, ProcMem> {
        let stale = self
            .last_sample_at
            .map(|t| t.elapsed() >= REFRESH)
            .unwrap_or(true);
        if stale {
            self.last_sample_at = Some(Instant::now());
            let (fresh, rss) = collect_top(procs, &self.cached, &self.last_rss);
            self.cached = fresh;
            self.last_rss = rss;
        }
        self.cached.clone()
    }
}

/// Detail for the top `MAX_PROCS` by RSS. Entries in `prev` whose RSS
/// matches `prev_rss` are carried over without a read. Returns the new
/// map and the RSS each entry was read (or carried) at.
fn collect_top(
    procs: &[ProcTick],
    prev: &HashMap<u32, ProcMem>,
    prev_rss: &HashMap<u32, u64>,
) -> (HashMap<u32, ProcMem>, HashMap<u32, u64>) {
    let mut by_rss: Vec<(u32, u64)> = procs.iter().map(|p| (p.pid, p.mem_rss)).collect();
    by_rss.sort_by_key(|&(_, rss)| std::cmp::Reverse(rss));
    let mut out = HashMap::new();
    let mut rss_seen = HashMap::new();
    for (pid, rss) in by_rss.into_iter().take(MAX_PROCS) {
        let reuse = prev_rss.get(&pid) == Some(&rss);
        let m = match prev.get(&pid) {
            Some(m) if reuse => *m,
            _ => collect_pid(pid),
        };
        if !m.is_empty() {
            out.insert(pid, m);
            rss_seen.insert(pid, rss);
        }
    }
    (out, rss_seen)
}

#[cfg(target_os = "linux")]
fn collect_pid(pid: u32) -> ProcMem {
    let Ok(text) = std::fs::read_to_string(format!("/proc/{}/smaps_rollup", pid)) else {
        return ProcMem::default();
    };
    parse_smaps_rollup(&text)
}

#[cfg(target_os = "macos")]
fn collect_pid(pid: u32) -> ProcMem {
    // SAFETY: proc_pid_rusage writes at most size_of::<rusage_info_v4>()
    // bytes for the V4 flavor into the zeroed buffer; we only read the
    // struct after the call reports success.
    unsafe {
        let mut info: libc::rusage_info_v4 = std::mem::zeroed();
        let ret = libc::proc_pid_rusage(
            pid as libc::c_int,
            libc::RUSAGE_INFO_V4,
            &mut info as *mut libc::rusage_info_v4 as *mut libc::rusage_info_t,
        );
        if ret != 0 {
            // EPERM for other users' procs without sudo, ESRCH for
            // procs that exited since the snapshot. Either way: no data.
            return ProcMem::default();
        }
        // ri_billed_energy is deliberately NOT read: empirically it
        // only moves at billing boundaries (nJ-scale deltas during a
        // busy loop), so it cannot back a live power readout.
        ProcMem {
            footprint: Some(info.ri_phys_footprint),
            peak: Some(info.ri_lifetime_max_phys_footprint),
            ..ProcMem::default()
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn collect_pid(_pid: u32) -> ProcMem {
    ProcMem::default()
}

/// Parse the totals out of a smaps_rollup file. Pure text → struct so
/// tests can drive it from synthetic strings on any host. Values are
/// `<n> kB` lines; unparseable lines are skipped.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn parse_smaps_rollup(text: &str) -> ProcMem {
    let mut pss = None;
    let mut private = 0u64;
    let mut shared = 0u64;
    let mut swap = None;
    let mut saw_private = false;
    let mut saw_shared = false;
    for line in text.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let Some(kb) = rest
            .split_whitespace()
            .next()
            .and_then(|n| n.parse::<u64>().ok())
        else {
            continue;
        };
        let bytes = kb.saturating_mul(1024);
        match key.trim() {
            "Pss" => pss = Some(bytes),
            "Private_Clean" | "Private_Dirty" => {
                private = private.saturating_add(bytes);
                saw_private = true;
            }
            "Shared_Clean" | "Shared_Dirty" => {
                shared = shared.saturating_add(bytes);
                saw_shared = true;
            }
            "Swap" => swap = Some(bytes),
            _ => {}
        }
    }
    ProcMem {
        pss,
        private: saw_private.then_some(private),
        shared: saw_shared.then_some(shared),
        swap,
        ..ProcMem::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_smaps_rollup_totals() {
        // Shape captured from a real 6.x kernel — the header line plus
        // `<Key>: <n> kB` rows.
        let sample = "\
00400000-7ffd13a3c000 ---p 00000000 00:00 0    [rollup]
Rss:              123456 kB
Pss:               45678 kB
Pss_Anon:          30000 kB
Shared_Clean:      60000 kB
Shared_Dirty:       2000 kB
Private_Clean:      5000 kB
Private_Dirty:     40000 kB
Referenced:       100000 kB
Anonymous:         42000 kB
Swap:               1234 kB
SwapPss:             617 kB
";
        let m = parse_smaps_rollup(sample);
        assert_eq!(m.pss, Some(45678 * 1024));
        assert_eq!(m.private, Some((5000 + 40000) * 1024));
        assert_eq!(m.shared, Some((60000 + 2000) * 1024));
        assert_eq!(m.swap, Some(1234 * 1024));
        assert_eq!(m.footprint, None);
    }

    #[test]
    fn empty_text_yields_no_data() {
        let m = parse_smaps_rollup("");
        assert!(m.is_empty());
        assert_eq!(m.private, None);
        assert_eq!(m.shared, None);
        assert_eq!(m.swap, None);
    }

    #[test]
    fn garbled_values_are_skipped() {
        let m = parse_smaps_rollup("Pss: not-a-number kB\nSwap: 10 kB\n");
        assert_eq!(m.pss, None);
        assert_eq!(m.swap, Some(10 * 1024));
    }

    #[test]
    fn zero_private_still_reports_some_when_keys_present() {
        // A process can legitimately have 0 private bytes; the column
        // should show "0 B", not "—".
        let m = parse_smaps_rollup("Private_Clean: 0 kB\nPrivate_Dirty: 0 kB\n");
        assert_eq!(m.private, Some(0));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn live_footprint_for_own_pid() {
        // proc_pid_rusage on our own PID never needs privileges, so
        // this exercises the real syscall path: a test process always
        // has a nonzero physical footprint.
        let m = collect_pid(std::process::id());
        assert!(m.footprint.unwrap_or(0) > 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn live_smaps_rollup_for_own_pid() {
        // Same idea on Linux — /proc/self/smaps_rollup is always
        // readable and a running test maps nonzero PSS.
        let m = collect_pid(std::process::id());
        assert!(m.pss.unwrap_or(0) > 0);
    }

    #[test]
    fn collector_caches_within_refresh_window() {
        let mut c = ProcMemCollector::new();
        let _ = c.sample(&[]);
        let first_at = c.last_sample_at;
        let _ = c.sample(&[]);
        assert_eq!(c.last_sample_at, first_at);
    }
}
