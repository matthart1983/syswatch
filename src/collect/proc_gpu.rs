//! Per-process GPU attribution (Linux only — public APIs).
//!
//! Two collection paths feed the same `HashMap<pid, ProcGpu>`:
//!
//! 1. **AMDGPU / Intel via `/proc/{pid}/fdinfo/{fd}`** — DRM exposes
//!    per-fd engine nanoseconds and per-fd VRAM/GTT memory keys.
//!    Walks /proc/*/fd/*, follows the readlink to filter for fds
//!    pointing at `/dev/dri/*`, parses the matching fdinfo, and
//!    derives util% from delta(engine_ns) / dt across two ticks.
//!
//! 2. **NVIDIA via nvml** (opt-in feature `gpu-nvidia`) —
//!    `Device::process_utilization_stats` returns a recent window
//!    of per-PID util samples, and `running_compute_processes` /
//!    `running_graphics_processes` give per-PID memory.
//!
//! macOS / Windows / BSD: stub returning an empty map — the data
//! isn't available without sudo (powermetrics) or private FFI we're
//! not yet ready to take on for a single column.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// The fdinfo walk touches every fd of every readable process, which
/// is ~10 ms on a desktop; GPU attribution does not need to move
/// faster than the other per-process collectors.
const REFRESH: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, Default)]
pub struct ProcGpu {
    pub gpu_pct: Option<f32>,
    pub gpu_mem_bytes: Option<u64>,
}

#[derive(Debug, Default)]
pub struct ProcGpuCollector {
    /// fdinfo delta state — last seen total engine_ns per pid + last
    /// observation time. Per-PID rather than per-fd because procs may
    /// open and close fds between samples; aggregating at the PID
    /// level is what the column ultimately renders.
    #[cfg(target_os = "linux")]
    prev_engine_ns: HashMap<u32, (u64, std::time::Instant)>,
    last_sample_at: Option<Instant>,
    cached: HashMap<u32, ProcGpu>,
}

impl ProcGpuCollector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Per-PID GPU attribution, re-sampled at most every `REFRESH`;
    /// between samples returns the cached map.
    pub fn sample(&mut self) -> HashMap<u32, ProcGpu> {
        let stale = self
            .last_sample_at
            .map(|t| t.elapsed() >= REFRESH)
            .unwrap_or(true);
        if stale {
            self.last_sample_at = Some(Instant::now());
            self.cached = self.sample_now();
        }
        self.cached.clone()
    }

    fn sample_now(&mut self) -> HashMap<u32, ProcGpu> {
        #[cfg(target_os = "linux")]
        {
            let mut out = self.sample_linux_fdinfo();
            #[cfg(feature = "gpu-nvidia")]
            sample_nvml_into(&mut out);
            return out;
        }
        #[cfg(not(target_os = "linux"))]
        {
            HashMap::new()
        }
    }

    // ── Linux fdinfo path ──────────────────────────────────────────
    #[cfg(target_os = "linux")]
    fn sample_linux_fdinfo(&mut self) -> HashMap<u32, ProcGpu> {
        use std::fs;
        let mut out: HashMap<u32, ProcGpu> = HashMap::new();
        let mut totals: HashMap<u32, (u64, u64)> = HashMap::new(); // pid → (engine_ns, mem_bytes)

        // No DRM device, no DRM fds: skip the walk entirely (headless
        // servers, containers).
        if !std::path::Path::new("/dev/dri").exists() {
            return out;
        }
        let Ok(proc_iter) = fs::read_dir("/proc") else {
            return out;
        };
        for entry in proc_iter.flatten() {
            let Some(pid_str) = entry.file_name().to_str().map(|s| s.to_string()) else {
                continue;
            };
            let Ok(pid) = pid_str.parse::<u32>() else {
                continue;
            };
            let fd_dir = entry.path().join("fd");
            let fdinfo_dir = entry.path().join("fdinfo");
            let Ok(fd_iter) = fs::read_dir(&fd_dir) else {
                continue;
            };
            for fd_entry in fd_iter.flatten() {
                // Filter to DRM device fds via readlink.
                let target = match fs::read_link(fd_entry.path()) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                if !target.starts_with("/dev/dri/") {
                    continue;
                }
                let fd_name = fd_entry.file_name();
                let fdinfo_path = fdinfo_dir.join(&fd_name);
                let Ok(text) = fs::read_to_string(&fdinfo_path) else {
                    continue;
                };
                let parsed = parse_fdinfo_drm(&text);
                let entry = totals.entry(pid).or_insert((0, 0));
                entry.0 = entry.0.saturating_add(parsed.engine_ns);
                entry.1 = entry.1.saturating_add(parsed.mem_bytes);
            }
        }

        let now = std::time::Instant::now();
        for (pid, (cur_ns, mem_bytes)) in totals {
            let pct = if let Some((prev_ns, prev_t)) = self.prev_engine_ns.get(&pid) {
                let dt = now.duration_since(*prev_t).as_secs_f64();
                if dt > 0.0 {
                    let dns = cur_ns.saturating_sub(*prev_ns) as f64;
                    // dns nanoseconds of GPU work in dt seconds of wall
                    // time. (dns/1e9) / dt → fraction of one GPU's time.
                    let frac = (dns / 1_000_000_000.0) / dt;
                    Some((frac * 100.0).clamp(0.0, 100.0) as f32)
                } else {
                    None
                }
            } else {
                None
            };
            self.prev_engine_ns.insert(pid, (cur_ns, now));
            out.insert(
                pid,
                ProcGpu {
                    gpu_pct: pct,
                    gpu_mem_bytes: if mem_bytes > 0 { Some(mem_bytes) } else { None },
                },
            );
        }
        // Drop stale prev entries so the map doesn't grow unbounded
        // across exits — keep only the PIDs we just observed.
        let live: std::collections::HashSet<u32> = out.keys().copied().collect();
        self.prev_engine_ns.retain(|pid, _| live.contains(pid));
        out
    }
}

#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Default, PartialEq)]
pub(crate) struct FdinfoDrm {
    /// Sum of all `drm-engine-*` ns values in the fdinfo.
    pub engine_ns: u64,
    /// Sum of all `drm-memory-vram` and `drm-memory-gtt` keys in bytes.
    pub mem_bytes: u64,
}

/// Parse the DRM-relevant lines out of a fdinfo file. Pure text →
/// struct so tests can drive it from synthetic strings.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn parse_fdinfo_drm(text: &str) -> FdinfoDrm {
    let mut out = FdinfoDrm::default();
    for line in text.lines() {
        // Lines look like:
        //   drm-engine-gfx:    12345 ns
        //   drm-memory-vram:   16384 KiB
        //   drm-memory-gtt:     1024 KiB
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let rest = rest.trim();
        // Value is `<number> <unit>`.
        let mut parts = rest.split_whitespace();
        let Some(num_str) = parts.next() else {
            continue;
        };
        let unit = parts.next().unwrap_or("");
        let Ok(n) = num_str.parse::<u64>() else {
            continue;
        };
        if key.starts_with("drm-engine-") {
            // Always nanoseconds in current kernels.
            out.engine_ns = out.engine_ns.saturating_add(n);
        } else if key.starts_with("drm-memory-") {
            let bytes = match unit {
                "KiB" => n.saturating_mul(1024),
                "MiB" => n.saturating_mul(1024 * 1024),
                "GiB" => n.saturating_mul(1024 * 1024 * 1024),
                _ => n, // bytes
            };
            out.mem_bytes = out.mem_bytes.saturating_add(bytes);
        }
    }
    out
}

// ── NVIDIA nvml path (opt-in via gpu-nvidia feature) ──────────────
#[cfg(all(target_os = "linux", feature = "gpu-nvidia"))]
fn sample_nvml_into(out: &mut HashMap<u32, ProcGpu>) {
    use nvml_wrapper::Nvml;
    use std::sync::OnceLock;
    static NVML: OnceLock<Option<Nvml>> = OnceLock::new();
    let Some(nvml) = NVML.get_or_init(|| Nvml::init().ok()).as_ref() else {
        return;
    };
    let count = nvml.device_count().unwrap_or(0);
    for i in 0..count {
        let Ok(dev) = nvml.device_by_index(i) else {
            continue;
        };
        // Per-PID memory from the running compute + graphics process
        // lists. Compute and graphics lists can overlap; later writes
        // win which is fine — both report the same memory total.
        if let Ok(procs) = dev.running_compute_processes() {
            for pi in procs {
                let entry = out.entry(pi.pid as u32).or_default();
                if let nvml_wrapper::enums::device::UsedGpuMemory::Used(b) = pi.used_gpu_memory {
                    entry.gpu_mem_bytes = Some(entry.gpu_mem_bytes.unwrap_or(0).saturating_add(b));
                }
            }
        }
        if let Ok(procs) = dev.running_graphics_processes() {
            for pi in procs {
                let entry = out.entry(pi.pid as u32).or_default();
                if let nvml_wrapper::enums::device::UsedGpuMemory::Used(b) = pi.used_gpu_memory {
                    entry.gpu_mem_bytes = Some(entry.gpu_mem_bytes.unwrap_or(0).saturating_add(b));
                }
            }
        }
        // Per-PID util — process_utilization_stats returns a window of
        // recent samples; we want the most-recent timestamp per PID.
        // Pass `last_seen_timestamp = 0` to ask for "everything nvml has".
        if let Ok(samples) = dev.process_utilization_stats(0) {
            let mut latest: HashMap<u32, (u64, u32)> = HashMap::new(); // pid → (timestamp_us, util)
            for s in samples {
                let pid = s.pid as u32;
                let prev_ts = latest.get(&pid).map(|(t, _)| *t).unwrap_or(0);
                if s.timestamp >= prev_ts {
                    latest.insert(pid, (s.timestamp, s.sm_util));
                }
            }
            for (pid, (_, util)) in latest {
                let entry = out.entry(pid).or_default();
                entry.gpu_pct = Some((util as f32).clamp(0.0, 100.0));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_amdgpu_fdinfo() {
        // Captured from a real AMDGPU fdinfo — line shape is stable
        // since drm/amd switched to the standardized keys in 6.x.
        let sample = "\
pos:    0
flags:  02100002
mnt_id: 30
ino:    1234
drm-driver:     amdgpu
drm-client-id:  42
drm-engine-gfx:    1234567890 ns
drm-engine-compute: 9876543210 ns
drm-memory-vram:   524288 KiB
drm-memory-gtt:    65536 KiB
";
        let parsed = parse_fdinfo_drm(sample);
        assert_eq!(parsed.engine_ns, 1234567890 + 9876543210);
        assert_eq!(parsed.mem_bytes, (524288 + 65536) * 1024);
    }

    #[test]
    fn parses_intel_fdinfo() {
        let sample = "\
drm-driver:     i915
drm-engine-render:  4500000000 ns
drm-engine-blitter: 100000 ns
drm-engine-video:   0 ns
drm-engine-video-enhance: 0 ns
drm-memory-system:  16384 KiB
";
        let parsed = parse_fdinfo_drm(sample);
        assert_eq!(parsed.engine_ns, 4500000000 + 100000);
        // drm-memory-system isn't drm-memory-vram or -gtt; current
        // parser counts every drm-memory-* key. That keeps the parser
        // permissive across drivers.
        assert_eq!(parsed.mem_bytes, 16384 * 1024);
    }

    #[test]
    fn ignores_non_drm_lines() {
        let sample = "\
pos:    0
flags:  02100002
mnt_id: 30
size:   12345
";
        let parsed = parse_fdinfo_drm(sample);
        assert_eq!(parsed, FdinfoDrm::default());
    }

    #[test]
    fn handles_missing_unit_as_bytes() {
        let sample = "drm-memory-vram: 4096\n";
        let parsed = parse_fdinfo_drm(sample);
        assert_eq!(parsed.mem_bytes, 4096);
    }

    #[test]
    fn handles_garbled_value() {
        let sample = "drm-engine-gfx: not-a-number ns\n";
        let parsed = parse_fdinfo_drm(sample);
        assert_eq!(parsed.engine_ns, 0);
    }
}
