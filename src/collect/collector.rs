use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Instant, SystemTime};

use sysinfo::{Disks, Networks, Pid, ProcessRefreshKind, RefreshKind, System, Users};

use super::gpu::GpuDiscovery;
#[cfg(target_os = "macos")]
use super::macos_sampler::MacosSampler;
use super::model::*;
use super::power::PowerCollector;
use super::proc_bandwidth::ProcessBandwidthCollector;
use super::proc_gpu::ProcGpuCollector;
use super::proc_memory::ProcMemCollector;
use super::sanitize::scrub_snapshot;
use super::services::ServicesCollector;

/// Collector keeps long-lived sysinfo handles + previous-tick counters so we can
/// compute rates. One instance per process; not Send across threads in the
/// current design (sysinfo handles aren't Sync).
pub struct Collector {
    sys: System,
    disks: Disks,
    nets: Networks,
    users: Users,
    last_tick: Option<Instant>,
    /// Last time we drove `sys.refresh_processes_specifics`. The
    /// process refresh is the most expensive sysinfo call (10–50 ms
    /// on a 1 k-proc box), so we cap its rate independently of the
    /// outer tick. Cached process state stays valid between
    /// refreshes — sysinfo's cpu_usage() returns the last computed
    /// delta until the next refresh.
    last_procs_refresh: Option<Instant>,
    last_disk_read: u64,
    last_disk_write: u64,
    last_iface: HashMap<String, (u64, u64)>, // name -> (rx, tx)
    last_proc_io: HashMap<u32, (u64, u64)>,  // pid -> cumulative (read, written) bytes
    gpu: GpuDiscovery,
    power: PowerCollector,
    proc_bw: ProcessBandwidthCollector,
    proc_gpu: ProcGpuCollector,
    proc_mem: ProcMemCollector,
    services: ServicesCollector,
    host: HostInfo,
    /// Shared IOReport + SMC sampler. Both `gpu` and `power` consume
    /// the per-tick output (`MacosTick`) so we only do one IOReport
    /// subscription + one SMC connection across the process.
    #[cfg(target_os = "macos")]
    macos: Option<MacosSampler>,
}

impl Collector {
    /// `tick_ms` is the UI sample rate from `SyswatchConfig`. On macOS
    /// it threads through to the IOReport+SMC sampler worker so the
    /// platform sample cadence matches the user's configured rate
    /// (clamped to a 250 ms floor — IOReport sampling has overhead).
    /// Other platforms ignore the value today.
    pub fn new(tick_ms: u64) -> Self {
        let _ = tick_ms; // suppress unused on non-macos

        let mut sys = System::new_with_specifics(RefreshKind::everything());
        sys.refresh_all();
        let disks = Disks::new_with_refreshed_list();
        let nets = Networks::new_with_refreshed_list();
        let users = Users::new_with_refreshed_list();

        let cpu_count = sys.cpus().len() as u32;
        let cpu_model = sys
            .cpus()
            .first()
            .map(|c| c.brand().to_string())
            .unwrap_or_default();
        let host = HostInfo {
            hostname: System::host_name().unwrap_or_else(|| "localhost".into()),
            os: format!(
                "{} {} {}",
                System::name().unwrap_or_else(|| "unknown".into()),
                System::os_version().unwrap_or_else(|| "".into()),
                std::env::consts::ARCH,
            ),
            uptime_secs: System::uptime(),
            cpu_model,
            cpu_cores: cpu_count,
        };

        Self {
            sys,
            disks,
            nets,
            users,
            last_tick: None,
            last_procs_refresh: None,
            last_disk_read: 0,
            last_disk_write: 0,
            last_iface: HashMap::new(),
            last_proc_io: HashMap::new(),
            gpu: GpuDiscovery::new(),
            power: PowerCollector::new(),
            proc_bw: ProcessBandwidthCollector::new(),
            proc_gpu: ProcGpuCollector::new(),
            proc_mem: ProcMemCollector::new(),
            services: ServicesCollector::new(),
            host,
            #[cfg(target_os = "macos")]
            macos: MacosSampler::try_init(tick_ms),
        }
    }

    #[allow(dead_code)]
    pub fn host(&self) -> &HostInfo {
        &self.host
    }

    pub fn sample(&mut self) -> Snapshot {
        let now = Instant::now();
        let dt_secs = self
            .last_tick
            .map(|t| (now - t).as_secs_f64().max(0.001))
            .unwrap_or(1.0);
        self.last_tick = Some(now);

        // sysinfo refresh: cpu/mem/processes/disks/networks. CPU and
        // memory are cheap and tick every iteration; the process list
        // is heavy and refreshes on its own ~1.5 s budget so the outer
        // loop can run at sub-second tick rates without paying for a
        // full process scan every frame.
        self.sys.refresh_cpu_all();
        self.sys.refresh_memory();
        const PROCS_REFRESH: std::time::Duration = std::time::Duration::from_millis(1500);
        let procs_stale = self
            .last_procs_refresh
            .map(|t| now.duration_since(t) >= PROCS_REFRESH)
            .unwrap_or(true);
        if procs_stale {
            self.sys.refresh_processes_specifics(
                sysinfo::ProcessesToUpdate::All,
                true,
                ProcessRefreshKind::everything(),
            );
            self.last_procs_refresh = Some(now);
        }
        self.disks.refresh();
        self.nets.refresh();

        let cpu = self.collect_cpu();
        let mem = self.collect_mem();
        let (disks, disk_io) = self.collect_disks(dt_secs);
        let net = self.collect_net(dt_secs);
        let mut procs = self.collect_procs(dt_secs);
        // Per-PID bandwidth — measured (nettop) on macOS, attributed
        // elsewhere. Cached at REFRESH inside the collector so we only
        // pay the subprocess cost a few times a second even at 1Hz
        // tick rates.
        let (bw, net_rates_estimated) = self.proc_bw.sample(&net);
        if !bw.is_empty() {
            for p in procs.iter_mut() {
                if let Some((rx, tx)) = bw.get(&p.pid) {
                    p.net_rx_rate = Some(*rx);
                    p.net_tx_rate = Some(*tx);
                }
            }
        }
        // Per-PID GPU attribution. Linux fdinfo (AMDGPU/Intel) plus
        // optional NVIDIA via nvml; macOS/Windows return empty.
        let pgpu = self.proc_gpu.sample();
        if !pgpu.is_empty() {
            for p in procs.iter_mut() {
                if let Some(g) = pgpu.get(&p.pid) {
                    p.gpu_pct = g.gpu_pct;
                    p.gpu_mem_bytes = g.gpu_mem_bytes;
                }
            }
        }
        // Per-PID memory detail (smaps_rollup / phys_footprint) for the
        // top procs by RSS. Cached inside the collector on its own
        // refresh budget like proc_bw / proc_gpu.
        let pmem = self.proc_mem.sample(&procs);
        if !pmem.is_empty() {
            for p in procs.iter_mut() {
                if let Some(m) = pmem.get(&p.pid) {
                    p.mem_footprint = m.footprint;
                    p.mem_pss = m.pss;
                    p.mem_private = m.private;
                    p.mem_shared = m.shared;
                    p.mem_swap = m.swap;
                    // Linux fills mem_peak from /proc/PID/status for
                    // every proc; only macOS supplies it here.
                    if m.peak.is_some() {
                        p.mem_peak = m.peak;
                    }
                }
            }
        }
        // Sample IOReport + SMC once per cycle on macOS; both `gpu` and
        // `power` read from the cached MacosTick so we don't duplicate
        // subscriptions or controller queries.
        #[cfg(target_os = "macos")]
        let macos_tick = self.macos.as_mut().and_then(|s| s.tick());
        #[cfg(target_os = "macos")]
        let macos_tick_ref = macos_tick.as_ref();

        // Estimated per-process power: the measured CPU-rail wattage
        // (IOReport) apportioned by each process's share of CPU time.
        // An estimate — rendered with `~` — but anchored to a real
        // measured total. Direct per-process energy is not readable
        // without sudo on either platform (ri_billed_energy only moves
        // at billing boundaries; RAPL is root-only since Platypus).
        #[cfg(target_os = "macos")]
        if let Some(cpu_w) = macos_tick_ref.and_then(|t| t.cpu_power_w) {
            let total_pct: f32 = procs.iter().map(|p| p.cpu_pct.max(0.0)).sum();
            if total_pct > 0.0 {
                for p in procs.iter_mut() {
                    if p.cpu_pct > 0.0 {
                        p.power_w = Some(cpu_w * p.cpu_pct / total_pct);
                    }
                }
            }
        }

        #[cfg(target_os = "macos")]
        let gpus = self.gpu.refresh(macos_tick_ref);
        #[cfg(not(target_os = "macos"))]
        let gpus = self.gpu.refresh();

        #[cfg(target_os = "macos")]
        let power = self.power.sample(macos_tick_ref);
        #[cfg(not(target_os = "macos"))]
        let power = self.power.sample();

        let services = self.services.sample();

        let mut host = self.host.clone();
        host.uptime_secs = System::uptime();

        let mut snap = Snapshot {
            t: SystemTime::now(),
            host,
            cpu,
            mem,
            disks,
            disk_io,
            net,
            procs,
            gpus,
            power,
            services,
            net_rates_estimated,
            pressure: collect_pressure(),
        };
        // Single choke point for terminal-escape scrubbing — every string
        // above came from the kernel or another user's process (issue #21).
        scrub_snapshot(&mut snap);
        snap
    }

    fn collect_cpu(&self) -> CpuTick {
        let load = System::load_average();
        let per_core: Vec<f32> = self.sys.cpus().iter().map(|c| c.cpu_usage()).collect();
        let usage_pct = if per_core.is_empty() {
            0.0
        } else {
            per_core.iter().sum::<f32>() / per_core.len() as f32
        };
        CpuTick {
            load_1: load.one as f32,
            load_5: load.five as f32,
            load_15: load.fifteen as f32,
            usage_pct,
            per_core,
        }
    }

    fn collect_mem(&self) -> MemTick {
        let total = self.sys.total_memory();
        let mut used = self.sys.used_memory();
        let mut available = self.sys.available_memory();
        // Subtract ZFS ARC from used (it's reclaimable cache, not committed).
        #[cfg(target_os = "linux")]
        {
            let arc = read_arc_size();
            (used, available) = arc_adjust(used, available, total, arc);
        }
        MemTick {
            total_bytes: total,
            used_bytes: used,
            available_bytes: available,
            swap_total_bytes: self.sys.total_swap(),
            swap_used_bytes: self.sys.used_swap(),
            pressure_level: mem_pressure_level(),
        }
    }

    fn collect_disks(&mut self, dt: f64) -> (Vec<DiskUsageTick>, DiskIoTick) {
        let mut out = Vec::new();
        for d in self.disks.iter() {
            let total = d.total_space();
            let avail = d.available_space();
            let used = total.saturating_sub(avail);
            let pct = if total > 0 {
                (used as f32) / (total as f32)
            } else {
                0.0
            };
            out.push(DiskUsageTick {
                mount_point: d.mount_point().to_string_lossy().into_owned(),
                device: d.name().to_string_lossy().into_owned(),
                fs_type: d.file_system().to_string_lossy().into_owned(),
                total_bytes: total,
                used_bytes: used,
                available_bytes: avail,
                usage_pct: pct * 100.0,
                read_only: d.is_read_only(),
            });
        }

        // Aggregate IO sources in preference order: netwatch-sdk's
        // /proc/diskstats reader (Linux), IOKit block-storage stats
        // (macOS — device-level truth incl. kernel/page-cache IO),
        // then summing per-process counters as the last resort.
        let (read_total, write_total) = netwatch_sdk::collectors::disk::collect_disk_io()
            .map(|io| (io.read_bytes, io.write_bytes))
            .or_else(|| {
                #[cfg(target_os = "macos")]
                {
                    super::disk_macos::collect_block_io()
                }
                #[cfg(not(target_os = "macos"))]
                {
                    None
                }
            })
            .unwrap_or_else(|| {
                let mut r = 0u64;
                let mut w = 0u64;
                for (_pid, p) in self.sys.processes() {
                    let io = p.disk_usage();
                    r = r.saturating_add(io.total_read_bytes);
                    w = w.saturating_add(io.total_written_bytes);
                }
                (r, w)
            });
        let read_rate = if self.last_disk_read == 0 {
            0.0
        } else {
            (read_total.saturating_sub(self.last_disk_read)) as f64 / dt
        };
        let write_rate = if self.last_disk_write == 0 {
            0.0
        } else {
            (write_total.saturating_sub(self.last_disk_write)) as f64 / dt
        };
        self.last_disk_read = read_total;
        self.last_disk_write = write_total;

        (
            out,
            DiskIoTick {
                read_bytes_total: read_total,
                write_bytes_total: write_total,
                read_rate,
                write_rate,
            },
        )
    }

    /// Net interfaces. On Linux we defer to netwatch-sdk so syswatch and
    /// netwatch-agent read byte counters through the same sysfs parser —
    /// there it's a cheap in-process read of `/proc/net/dev`.
    ///
    /// macOS deliberately does NOT use the SDK here: its
    /// `collect_interface_stats()` shells out to `netstat -ibn` plus an
    /// `ifconfig <iface>` per interface on every call — a dozen-plus
    /// `posix_spawn`s per tick, which was the single largest contributor to
    /// syswatch's CPU usage (issue #4). sysinfo already refreshes the same
    /// kernel counters in-process via `getifaddrs` (`self.nets.refresh()`
    /// runs every tick), so on macOS we read those instead and pay nothing
    /// extra. Everywhere else also falls through to the sysinfo path.
    fn collect_net(&mut self, dt: f64) -> Vec<InterfaceTick> {
        let mut out = Vec::new();

        #[cfg(target_os = "linux")]
        if let Ok(stats) = netwatch_sdk::platform::collect_interface_stats() {
            for (name, s) in stats {
                let prev = self.last_iface.get(&name).copied().unwrap_or((0, 0));
                let rx_rate = if prev.0 == 0 {
                    0.0
                } else {
                    s.rx_bytes.saturating_sub(prev.0) as f64 / dt
                };
                let tx_rate = if prev.1 == 0 {
                    0.0
                } else {
                    s.tx_bytes.saturating_sub(prev.1) as f64 / dt
                };
                self.last_iface
                    .insert(name.clone(), (s.rx_bytes, s.tx_bytes));
                out.push(InterfaceTick {
                    name,
                    is_up: s.is_up,
                    rx_bytes: s.rx_bytes,
                    tx_bytes: s.tx_bytes,
                    rx_rate,
                    tx_rate,
                });
            }
            out.sort_by(|a, b| a.name.cmp(&b.name));
            return out;
        }

        // Cross-platform fallback via sysinfo (also the macOS path now).
        // sysinfo exposes byte counters but not interface up/down state, so
        // on macOS we layer in a single in-process `getifaddrs` flag read —
        // no `ifconfig` fork. Other platforms can't tell and report `true`.
        #[cfg(target_os = "macos")]
        let up_flags = interface_up_flags();
        for (name, data) in self.nets.iter() {
            let rx = data.total_received();
            let tx = data.total_transmitted();
            let prev = self.last_iface.get(name).copied().unwrap_or((0, 0));
            let rx_rate = if prev.0 == 0 {
                0.0
            } else {
                rx.saturating_sub(prev.0) as f64 / dt
            };
            let tx_rate = if prev.1 == 0 {
                0.0
            } else {
                tx.saturating_sub(prev.1) as f64 / dt
            };
            self.last_iface.insert(name.clone(), (rx, tx));
            #[cfg(target_os = "macos")]
            let is_up = up_flags.get(name).copied().unwrap_or(true);
            #[cfg(not(target_os = "macos"))]
            let is_up = true;
            out.push(InterfaceTick {
                name: name.clone(),
                is_up,
                rx_bytes: rx,
                tx_bytes: tx,
                rx_rate,
                tx_rate,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    fn collect_procs(&mut self, dt: f64) -> Vec<ProcTick> {
        let mut next_io: HashMap<u32, (u64, u64)> =
            HashMap::with_capacity(self.sys.processes().len());
        let mut out: Vec<ProcTick> = Vec::with_capacity(self.sys.processes().len());

        for (pid, p) in self.sys.processes() {
            // sysinfo lists Linux tasks (threads) as separate processes.
            // A thread shares its group's memory map, so keeping them
            // repeats the same RSS/PSS once per thread — the main row
            // already carries whole-process CPU and memory.
            if p.thread_kind().is_some() {
                continue;
            }
            let pid_u = pid.as_u32();
            let io = p.disk_usage();
            let cumulative = (io.total_read_bytes, io.total_written_bytes);
            let prev = self.last_proc_io.get(&pid_u).copied().unwrap_or(cumulative);
            let io_read_rate = (cumulative.0.saturating_sub(prev.0)) as f64 / dt;
            let io_write_rate = (cumulative.1.saturating_sub(prev.1)) as f64 / dt;
            next_io.insert(pid_u, cumulative);

            let user = p
                .user_id()
                .and_then(|uid| self.users.get_user_by_id(uid))
                .map(|u| u.name().to_string())
                .unwrap_or_else(|| "?".into());
            let (threads, mem_peak) = proc_threads_and_peak(pid_u);
            out.push(ProcTick {
                pid: pid_u,
                ppid: p.parent().map(Pid::as_u32).unwrap_or(0),
                user,
                name: p.name().to_string_lossy().into_owned(),
                cmd: p
                    .cmd()
                    .iter()
                    .map(|s| s.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join(" "),
                cpu_pct: p.cpu_usage(),
                mem_rss: p.memory(),
                mem_virt: p.virtual_memory(),
                threads,
                state: status_to_char(p.status()),
                start_time: Some(
                    SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(p.start_time()),
                ),
                io_read_rate,
                io_write_rate,
                // Filled in by Collector::sample after collect_procs
                // returns — proc_bandwidth + proc_gpu + proc_mem lookups
                // are per-tick.
                net_rx_rate: None,
                net_tx_rate: None,
                gpu_pct: None,
                gpu_mem_bytes: None,
                mem_footprint: None,
                mem_pss: None,
                mem_private: None,
                mem_shared: None,
                mem_swap: None,
                mem_peak,
                power_w: None,
            });
        }
        self.last_proc_io = next_io;

        out.sort_by(|a, b| {
            b.cpu_pct
                .partial_cmp(&a.cpu_pct)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        out
    }
}

/// Real thread count plus (on Linux) lifetime peak RSS, from one read
/// of `/proc/PID/status`. macOS peak comes from rusage in proc_memory;
/// thread counts come from proc_pidinfo here.
#[cfg(target_os = "linux")]
fn proc_threads_and_peak(pid: u32) -> (Option<u32>, Option<u64>) {
    let Ok(text) = std::fs::read_to_string(format!("/proc/{}/status", pid)) else {
        return (None, None);
    };
    parse_status_threads_peak(&text)
}

#[cfg(any(target_os = "linux", test))]
fn parse_status_threads_peak(text: &str) -> (Option<u32>, Option<u64>) {
    let mut threads = None;
    let mut peak = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Threads:") {
            threads = rest.trim().parse::<u32>().ok();
        } else if let Some(rest) = line.strip_prefix("VmHWM:") {
            // `VmHWM:    2412 kB`
            peak = rest
                .split_whitespace()
                .next()
                .and_then(|n| n.parse::<u64>().ok())
                .map(|kb| kb.saturating_mul(1024));
        }
        if threads.is_some() && peak.is_some() {
            break;
        }
    }
    (threads, peak)
}

#[cfg(target_os = "macos")]
fn proc_threads_and_peak(pid: u32) -> (Option<u32>, Option<u64>) {
    // SAFETY: proc_pidinfo writes at most size_of::<proc_taskinfo>()
    // bytes; we check the returned size before reading. Fails with
    // EPERM for other users' procs without sudo → None.
    unsafe {
        let mut ti: libc::proc_taskinfo = std::mem::zeroed();
        let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
        let ret = libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTASKINFO,
            0,
            &mut ti as *mut libc::proc_taskinfo as *mut libc::c_void,
            size,
        );
        if ret < size {
            return (None, None);
        }
        (Some(ti.pti_threadnum.max(0) as u32), None)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn proc_threads_and_peak(_pid: u32) -> (Option<u32>, Option<u64>) {
    (None, None)
}

/// PSI from /proc/pressure (Linux ≥4.20 with CONFIG_PSI). None when
/// the files are absent or unreadable.
#[cfg(target_os = "linux")]
fn collect_pressure() -> Option<PressureTick> {
    let read = |res: &str| std::fs::read_to_string(format!("/proc/pressure/{}", res)).ok();
    let cpu = parse_psi_avg10(&read("cpu")?);
    let mem = parse_psi_avg10(&read("memory")?);
    let io = parse_psi_avg10(&read("io")?);
    Some(PressureTick {
        cpu_some: cpu.0,
        mem_some: mem.0,
        mem_full: mem.1,
        io_some: io.0,
        io_full: io.1,
    })
}

#[cfg(not(target_os = "linux"))]
fn collect_pressure() -> Option<PressureTick> {
    None
}

fn arc_adjust(used: u64, available: u64, total: u64, arc_bytes: u64) -> (u64, u64) {
    let moved = arc_bytes.min(used);
    (used - moved, (available + moved).min(total))
}

/// Parse the `size` (resident ARC size) field from /proc/spl/kstat/zfs/arcstats text.
fn parse_arcstats_size(text: &str) -> Option<u64> {
    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.first() == Some(&"size") {
            return parts.get(2).and_then(|v| v.parse::<u64>().ok());
        }
    }
    None
}

/// Cached at startup — ZFS modules don't load/unload at runtime.
static HAS_ZFS: OnceLock<bool> = OnceLock::new();

#[cfg(target_os = "linux")]
fn read_arc_size() -> u64 {
    if !*HAS_ZFS.get_or_init(|| {
        std::path::Path::new("/proc/spl/kstat/zfs/arcstats").exists()
    }) {
        return 0;
    }
    std::fs::read_to_string("/proc/spl/kstat/zfs/arcstats")
        .ok()
        .and_then(|s| parse_arcstats_size(&s))
        .unwrap_or(0)
}

#[cfg(not(target_os = "linux"))]
fn read_arc_size() -> u64 {
    0
}

/// macOS's own memory-pressure verdict, from
/// `kern.memorystatus_vm_pressure_level`. Readable without elevation.
///
/// The values are the `DISPATCH_MEMORYPRESSURE_*` bits out of
/// `<dispatch/source.h>` — 0x01 normal, 0x02 warn, 0x04 critical — tested
/// most-severe-first because they are a bitmask, not an ordinal.
#[cfg(target_os = "macos")]
fn mem_pressure_level() -> Option<MemPressureLevel> {
    let mut level: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>();
    // SAFETY: the name is NUL-terminated, and `oldp`/`oldlenp` point at a
    // correctly-sized int for the duration of the call.
    let rc = unsafe {
        libc::sysctlbyname(
            c"kern.memorystatus_vm_pressure_level".as_ptr(),
            &mut level as *mut libc::c_int as *mut libc::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    if level & 0x04 != 0 {
        Some(MemPressureLevel::Critical)
    } else if level & 0x02 != 0 {
        Some(MemPressureLevel::Warning)
    } else if level & 0x01 != 0 {
        Some(MemPressureLevel::Normal)
    } else {
        // A level we don't recognise is not a licence to guess.
        None
    }
}

#[cfg(not(target_os = "macos"))]
fn mem_pressure_level() -> Option<MemPressureLevel> {
    None
}

/// (some_avg10, full_avg10) out of a /proc/pressure file:
/// ```text
/// some avg10=0.12 avg60=0.05 avg300=0.01 total=12345
/// full avg10=0.03 avg60=0.01 avg300=0.00 total=6789
/// ```
/// The cpu file may omit the `full` line on older kernels → 0.0.
#[cfg(any(target_os = "linux", test))]
fn parse_psi_avg10(text: &str) -> (f32, f32) {
    let mut some = 0.0;
    let mut full = 0.0;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let kind = parts.next().unwrap_or("");
        let avg10 = parts
            .next()
            .and_then(|t| t.strip_prefix("avg10="))
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(0.0);
        match kind {
            "some" => some = avg10,
            "full" => full = avg10,
            _ => {}
        }
    }
    (some, full)
}

fn status_to_char(s: sysinfo::ProcessStatus) -> char {
    // ProcessStatus's Display impl yields short codes ("R", "S", "Z", …) on
    // every platform — sidestep the per-OS variant set.
    s.to_string()
        .chars()
        .next()
        .unwrap_or('?')
        .to_ascii_uppercase()
}

/// macOS interface up/down map via a single in-process `getifaddrs` call.
///
/// sysinfo gives byte counters but not link state, and the old SDK path
/// recovered state by forking `ifconfig <iface>` once per interface every
/// tick (issue #4). `getifaddrs` returns the `IFF_UP | IFF_RUNNING` flags
/// for every interface in one syscall instead. An interface appears once
/// per address family, all sharing the same flags, so we OR the result and
/// last-write is harmless. On failure the map is empty and callers default
/// to `true`.
#[cfg(target_os = "macos")]
fn interface_up_flags() -> HashMap<String, bool> {
    use std::ffi::CStr;

    let mut map = HashMap::new();
    // SAFETY: standard getifaddrs/freeifaddrs ownership dance. We only read
    // fields while the list is alive and free it before returning.
    unsafe {
        let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifap) != 0 {
            return map;
        }
        let mut cur = ifap;
        while !cur.is_null() {
            let ifa = &*cur;
            if !ifa.ifa_name.is_null() {
                let name = CStr::from_ptr(ifa.ifa_name).to_string_lossy().into_owned();
                let flags = ifa.ifa_flags;
                let up =
                    (flags & libc::IFF_UP as u32) != 0 && (flags & libc::IFF_RUNNING as u32) != 0;
                let entry = map.entry(name).or_insert(false);
                *entry |= up;
            }
            cur = ifa.ifa_next;
        }
        libc::freeifaddrs(ifap);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn psi_parses_some_and_full() {
        let text = "\
some avg10=1.25 avg60=0.50 avg300=0.10 total=12345
full avg10=0.40 avg60=0.20 avg300=0.05 total=6789
";
        assert_eq!(parse_psi_avg10(text), (1.25, 0.40));
    }

    #[test]
    fn psi_cpu_file_without_full_line() {
        // Older kernels omit `full` for cpu — must default to 0, not error.
        let text = "some avg10=3.00 avg60=1.00 avg300=0.20 total=999\n";
        assert_eq!(parse_psi_avg10(text), (3.00, 0.0));
    }

    #[test]
    fn psi_garbled_yields_zeros() {
        assert_eq!(parse_psi_avg10("nonsense\n"), (0.0, 0.0));
        assert_eq!(parse_psi_avg10(""), (0.0, 0.0));
    }

    #[test]
    fn status_parses_threads_and_peak() {
        let text = "\
Name:\tsyswatch
Umask:\t0002
State:\tS (sleeping)
VmPeak:\t  920000 kB
VmHWM:\t    2412 kB
VmRSS:\t    2400 kB
Threads:\t17
";
        let (threads, peak) = parse_status_threads_peak(text);
        assert_eq!(threads, Some(17));
        assert_eq!(peak, Some(2412 * 1024));
    }

    #[test]
    fn status_missing_fields_yield_none() {
        let (threads, peak) = parse_status_threads_peak("Name:\tx\n");
        assert_eq!(threads, None);
        assert_eq!(peak, None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn live_status_for_own_pid() {
        let (threads, peak) = proc_threads_and_peak(std::process::id());
        assert!(threads.unwrap_or(0) >= 1);
        assert!(peak.unwrap_or(0) > 0);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn live_taskinfo_for_own_pid() {
        let (threads, peak) = proc_threads_and_peak(std::process::id());
        assert!(threads.unwrap_or(0) >= 1);
        // Peak comes from rusage in proc_memory on macOS, not here.
        assert_eq!(peak, None);
    }

    // ── ZFS ARC adjustments ────────────────────────────────────────────

    #[test]
    fn arc_adjust_no_arc_preserves_values() {
        // Zero ARC → no change to used or available.
        assert_eq!(arc_adjust(100, 50, 150, 0), (100, 50));
    }

    #[test]
    fn arc_adjust_subtracts_from_used_adds_to_available() {
        // 30 bytes of ARC moved from used → available.
        assert_eq!(arc_adjust(100, 50, 150, 30), (70, 80));
    }

    #[test]
    fn arc_adjust_clamps_when_arc_exceeds_used() {
        // ARC larger than used: used clamps to 0, available clamps to total.
        assert_eq!(arc_adjust(100, 50, 150, 150), (0, 150));
    }

    #[test]
    fn parse_arcstats_size_extracts_size_from_real_output() {
        let text = "\
24 1 0x01 148 40256 5856356141 1344751450507729
name                            type data
hits                            4    2049659748
iohits                          4    1195048
misses                          4    4407338
c                               4    66373946864
c_min                           4    4217798400
c_max                           4    133895806976
size                            4    65967562640";
        assert_eq!(parse_arcstats_size(text), Some(65967562640));
    }

    #[test]
    fn parse_arcstats_size_returns_none_when_size_absent() {
        assert_eq!(parse_arcstats_size("name  type  data\nhits  4  100\n"), None);
    }

    #[test]
    fn parse_arcstats_size_returns_none_on_empty() {
        assert_eq!(parse_arcstats_size(""), None);
    }
}
