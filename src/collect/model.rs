// Models are the wire shape between collectors and the UI. Some fields
// (cpu_cores, threads) are populated for completeness even when no tab
// reads them yet — adding a column is a UI change, not a model change.
#![allow(dead_code)]

use std::time::SystemTime;

#[derive(Debug, Clone, Default)]
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
    pub usage_pct: f32,     // aggregate 0..100
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ServiceStatus {
    Running,
    Idle,
    Failed,
    #[default]
    Unknown,
}

impl ServiceStatus {
    pub fn label(self) -> &'static str {
        match self {
            ServiceStatus::Running => "Running",
            ServiceStatus::Idle => "Idle",
            ServiceStatus::Failed => "Failed",
            ServiceStatus::Unknown => "Unknown",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ServiceTick {
    pub name: String,
    pub status: ServiceStatus,
    pub pid: Option<u32>,
    pub exit_code: Option<i32>,
    /// Free-form per-platform detail: systemd's SUB+DESCRIPTION, or
    /// launchctl's raw status code, useful in the drill-in.
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PowerSource {
    Ac,
    Battery,
    #[default]
    Unknown,
}

impl PowerSource {
    pub fn label(self) -> &'static str {
        match self {
            PowerSource::Ac => "AC",
            PowerSource::Battery => "Battery",
            PowerSource::Unknown => "Unknown",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct BatteryTick {
    pub charge_pct: f32, // 0..100
    pub is_charging: bool,
    pub fully_charged: bool,
    pub time_remaining_min: Option<u32>,
    pub cycle_count: Option<u32>,
    pub health_pct: Option<f32>, // current_max / design_max * 100
    pub temp_c: Option<f32>,
    pub voltage_v: Option<f32>,
    pub amperage_ma: Option<i32>, // signed: positive = charging, negative = discharging
}

#[derive(Debug, Clone, Default)]
pub struct ThermalZone {
    pub name: String,
    pub temp_c: f32,
}

#[derive(Debug, Clone, Default)]
pub struct FanTick {
    pub name: String,
    pub rpm: u32,
    pub target_rpm: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct PowerTick {
    pub source: PowerSource,
    pub battery: Option<BatteryTick>,
    /// 0..100 — % of nominal CPU speed available. <100 indicates thermal
    /// throttling. None when the platform doesn't expose it.
    pub thermal_throttle_pct: Option<u32>,
    pub thermal_zones: Vec<ThermalZone>,
    pub fans: Vec<FanTick>,
    /// System-wide power draw in watts, derived from battery V*A on macOS or
    /// from /sys/class/power_supply on Linux. None when on AC and the platform
    /// can't measure draw without sudo (typical on macOS Apple Silicon).
    pub system_power_w: Option<f32>,
    pub live_data_hint: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct GpuTick {
    pub name: String,
    pub vendor: String,
    pub driver: Option<String>,
    pub vram_total_bytes: Option<u64>,
    pub vram_used_bytes: Option<u64>,
    pub util_pct: Option<f32>, // 0..100
    pub temp_c: Option<f32>,
    pub power_w: Option<f32>,
    /// What the user can do to get live util/temp/power if it's currently
    /// missing. Empty when live data is already available.
    pub live_data_hint: Option<String>,
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
    pub io_rate: f64, // bytes/sec read + written, computed against previous tick
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
    pub gpus: Vec<GpuTick>,
    pub power: PowerTick,
    pub services: Vec<ServiceTick>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            t: SystemTime::UNIX_EPOCH,
            host: HostInfo::default(),
            cpu: CpuTick::default(),
            mem: MemTick::default(),
            disks: Vec::new(),
            disk_io: DiskIoTick::default(),
            net: Vec::new(),
            procs: Vec::new(),
            gpus: Vec::new(),
            power: PowerTick::default(),
            services: Vec::new(),
        }
    }
}
