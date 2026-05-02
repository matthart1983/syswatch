use std::collections::HashMap;
use std::time::{Instant, SystemTime};

use sysinfo::{Disks, Networks, Pid, ProcessRefreshKind, RefreshKind, System, Users};

use super::model::*;

/// Collector keeps long-lived sysinfo handles + previous-tick counters so we can
/// compute rates. One instance per process; not Send across threads in the
/// current design (sysinfo handles aren't Sync).
pub struct Collector {
    sys: System,
    disks: Disks,
    nets: Networks,
    users: Users,
    last_tick: Option<Instant>,
    last_disk_read: u64,
    last_disk_write: u64,
    last_iface: HashMap<String, (u64, u64)>, // name -> (rx, tx)
    host: HostInfo,
}

impl Collector {
    pub fn new() -> Self {
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
            last_disk_read: 0,
            last_disk_write: 0,
            last_iface: HashMap::new(),
            host,
        }
    }

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

        // sysinfo refresh: cpu/mem/processes/disks/networks.
        self.sys.refresh_cpu_all();
        self.sys.refresh_memory();
        self.sys.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::everything(),
        );
        self.disks.refresh();
        self.nets.refresh();

        let cpu = self.collect_cpu();
        let mem = self.collect_mem();
        let (disks, disk_io) = self.collect_disks(dt_secs);
        let net = self.collect_net(dt_secs);
        let procs = self.collect_procs();

        let mut host = self.host.clone();
        host.uptime_secs = System::uptime();

        Snapshot {
            t: SystemTime::now(),
            host,
            cpu,
            mem,
            disks,
            disk_io,
            net,
            procs,
            live: true,
        }
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
        MemTick {
            total_bytes: self.sys.total_memory(),
            used_bytes: self.sys.used_memory(),
            available_bytes: self.sys.available_memory(),
            swap_total_bytes: self.sys.total_swap(),
            swap_used_bytes: self.sys.used_swap(),
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
            });
        }

        // Aggregate per-process IO into a host-wide total — sysinfo doesn't
        // expose per-device IO portably, so this is our best xplat proxy.
        let mut read_total = 0u64;
        let mut write_total = 0u64;
        for (_pid, p) in self.sys.processes() {
            let io = p.disk_usage();
            read_total = read_total.saturating_add(io.total_read_bytes);
            write_total = write_total.saturating_add(io.total_written_bytes);
        }
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

    /// Net interfaces. Where the platform SDK supports it, defer to
    /// netwatch-sdk so syswatch and netwatch-agent see byte counters through
    /// the same parsers (sysfs on Linux, getifaddrs on macOS). On Linux/macOS
    /// we use the SDK; everywhere else fall back to sysinfo.
    fn collect_net(&mut self, dt: f64) -> Vec<InterfaceTick> {
        let mut out = Vec::new();

        #[cfg(any(target_os = "linux", target_os = "macos"))]
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
                self.last_iface.insert(name.clone(), (s.rx_bytes, s.tx_bytes));
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

        // Cross-platform fallback via sysinfo.
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
            out.push(InterfaceTick {
                name: name.clone(),
                is_up: true,
                rx_bytes: rx,
                tx_bytes: tx,
                rx_rate,
                tx_rate,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    fn collect_procs(&self) -> Vec<ProcTick> {
        let mut out: Vec<ProcTick> = self
            .sys
            .processes()
            .iter()
            .map(|(pid, p)| {
                let user = p
                    .user_id()
                    .and_then(|uid| self.users.get_user_by_id(uid))
                    .map(|u| u.name().to_string())
                    .unwrap_or_else(|| "?".into());
                ProcTick {
                    pid: pid.as_u32(),
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
                    threads: 1, // sysinfo doesn't expose thread count portably
                    state: status_to_char(p.status()),
                    start_time: Some(
                        SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(p.start_time()),
                    ),
                }
            })
            .collect();
        out.sort_by(|a, b| {
            b.cpu_pct
                .partial_cmp(&a.cpu_pct)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        out
    }
}

fn status_to_char(s: sysinfo::ProcessStatus) -> char {
    // ProcessStatus's Display impl yields short codes ("R", "S", "Z", …) on
    // every platform — sidestep the per-OS variant set.
    s.to_string().chars().next().unwrap_or('?').to_ascii_uppercase()
}
