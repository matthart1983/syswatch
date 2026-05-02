use std::time::SystemTime;

#[derive(Debug, Clone)]
pub struct HostInfo {
    pub hostname: String,
    pub os: String,
    pub uptime_secs: u64,
    pub cpu_model: String,
    pub cpu_cores: u32,
}

#[derive(Debug, Clone, Default)]
pub struct CpuTick {
    pub load_1: f32,
    pub load_5: f32,
    pub load_15: f32,
    pub usage_pct: f32, // aggregate 0..100
    pub per_core: Vec<f32>, // 0..100 per logical core
}

#[derive(Debug, Clone, Default)]
pub struct MemTick {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
}

#[derive(Debug, Clone, Default)]
pub struct DiskUsageTick {
    pub mount_point: String,
    pub device: String,
    pub fs_type: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub usage_pct: f32,
}

#[derive(Debug, Clone, Default)]
pub struct DiskIoTick {
    pub read_bytes_total: u64,
    pub write_bytes_total: u64,
    pub read_rate: f64,
    pub write_rate: f64,
}

#[derive(Debug, Clone, Default)]
pub struct InterfaceTick {
    pub name: String,
    pub is_up: bool,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_rate: f64,
    pub tx_rate: f64,
}

#[derive(Debug, Clone, Default)]
pub struct ProcTick {
    pub pid: u32,
    pub ppid: u32,
    pub user: String,
    pub name: String,
    pub cmd: String,
    pub cpu_pct: f32,
    pub mem_rss: u64,
    pub mem_virt: u64,
    pub threads: u32,
    pub state: char,
    pub start_time: Option<SystemTime>,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub t: SystemTime,
    pub host: HostInfo,
    pub cpu: CpuTick,
    pub mem: MemTick,
    pub disks: Vec<DiskUsageTick>,
    pub disk_io: DiskIoTick,
    pub net: Vec<InterfaceTick>,
    pub procs: Vec<ProcTick>,
    pub live: bool,
}
