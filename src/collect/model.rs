// Models are the wire shape between collectors and the UI. Some fields
// (cpu_cores, threads) are populated for completeness even when no tab
// reads them yet — adding a column is a UI change, not a model change.
#![allow(dead_code)]

use std::time::SystemTime;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct HostInfo {
    pub hostname: String,
    pub os: String,
    pub uptime_secs: u64,
    pub cpu_model: String,
    pub cpu_cores: u32,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CpuTick {
    pub load_1: f32,
    pub load_5: f32,
    pub load_15: f32,
    pub usage_pct: f32,     // aggregate 0..100
    pub per_core: Vec<f32>, // 0..100 per logical core
}

/// The kernel's own verdict on memory pressure, where the platform has one.
///
/// This is deliberately a level rather than a percentage: macOS reports a
/// three-state enum, and inventing PSI-shaped percentages from it would be a
/// fabricated number wearing a real one's clothes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MemPressureLevel {
    Normal,
    Warning,
    Critical,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct MemTick {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
    /// macOS `kern.memorystatus_vm_pressure_level` — the same authority PSI
    /// is on Linux, and the only way to tell a Mac that is genuinely short of
    /// RAM from one merely doing what macOS always does (holding memory full
    /// and keeping a swap file). `None` where the platform has no such
    /// signal; `#[serde(default)]` keeps older snapshots deserializing.
    #[serde(default)]
    pub pressure_level: Option<MemPressureLevel>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DiskUsageTick {
    pub mount_point: String,
    pub device: String,
    pub fs_type: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub usage_pct: f32,
    /// Mounted read-only (e.g. composefs/overlay roots on immutable distros,
    /// squashfs, iso9660). A read-only filesystem at 100% is normal and
    /// unactionable, so capacity warnings skip it (issue #9). `#[serde(default)]`
    /// keeps older snapshots deserializing.
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DiskIoTick {
    pub read_bytes_total: u64,
    pub write_bytes_total: u64,
    pub read_rate: f64,
    pub write_rate: f64,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct InterfaceTick {
    pub name: String,
    pub is_up: bool,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_rate: f64,
    pub tx_rate: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
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

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ServiceTick {
    pub name: String,
    pub status: ServiceStatus,
    pub pid: Option<u32>,
    pub exit_code: Option<i32>,
    /// Free-form per-platform detail: systemd's SUB+DESCRIPTION, or
    /// launchctl's raw status code, useful in the drill-in.
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
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

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
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

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ThermalZone {
    pub name: String,
    pub temp_c: f32,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct FanTick {
    pub name: String,
    pub rpm: u32,
    pub target_rpm: Option<u32>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
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

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct GpuTick {
    pub name: String,
    pub vendor: String,
    pub driver: Option<String>,
    pub vram_total_bytes: Option<u64>,
    pub vram_used_bytes: Option<u64>,
    pub util_pct: Option<f32>, // 0..100
    /// Apple Silicon split — fragment shader / fixed-function rasterizer
    /// load. Sums roughly to `util_pct`. None on platforms that don't
    /// expose the breakdown.
    pub renderer_util_pct: Option<f32>,
    pub tiler_util_pct: Option<f32>,
    pub temp_c: Option<f32>,
    pub power_w: Option<f32>,
    /// What the user can do to get live util/temp/power if it's currently
    /// missing. Empty when live data is already available.
    pub live_data_hint: Option<String>,
    /// Apple Silicon only — the most recent PID to submit GPU work,
    /// from ioreg's `AGCInfo.fLastSubmissionPID`. A rotating hint
    /// rather than a usage metric (the kernel doesn't publish per-PID
    /// cumulative GPU time without sudo or private FFI), but it
    /// answers "anything actively touching the GPU?" which is the
    /// most you can get on macOS without those.
    pub last_submitter_pid: Option<u32>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ProcTick {
    pub pid: u32,
    pub ppid: u32,
    pub user: String,
    pub name: String,
    pub cmd: String,
    pub cpu_pct: f32,
    pub mem_rss: u64,
    pub mem_virt: u64,
    /// Real thread count — /proc/PID/status `Threads:` on Linux,
    /// proc_pidinfo(PROC_PIDTASKINFO) on macOS. None when the platform
    /// call fails (other users' procs on macOS without sudo).
    pub threads: Option<u32>,
    pub state: char,
    pub start_time: Option<SystemTime>,
    /// Disk IO split by direction, bytes/sec against the previous tick.
    pub io_read_rate: f64,
    pub io_write_rate: f64,
    /// Per-process network rates in bytes/sec. Measured per-PID kernel
    /// counters via nettop on macOS; elsewhere an estimate that splits
    /// non-loopback interface throughput by connection count (see
    /// `Snapshot::net_rates_estimated`). None when neither source has
    /// data for this proc.
    pub net_rx_rate: Option<f64>,
    pub net_tx_rate: Option<f64>,
    /// Per-process GPU utilization (0..100). Sourced from nvml on
    /// Linux NVIDIA, /proc/PID/fdinfo drm-engine-* deltas on Linux
    /// AMDGPU/Intel, None on macOS/Windows (no public per-PID API
    /// without sudo or private FFI).
    pub gpu_pct: Option<f32>,
    /// Per-process GPU memory bytes — VRAM held by the process. Same
    /// platform availability as `gpu_pct`.
    pub gpu_mem_bytes: Option<u64>,
    /// Deeper memory attribution from `ProcMemCollector` — sampled for
    /// the top procs by RSS only, so most rows carry None. macOS:
    /// phys_footprint (Activity Monitor's Memory column). None elsewhere.
    pub mem_footprint: Option<u64>,
    /// Linux PSS — shared pages divided by mapper count, so PSS sums
    /// to the real footprint where RSS double-counts. None elsewhere.
    pub mem_pss: Option<u64>,
    /// Linux private (clean+dirty) — what frees if the process exits.
    pub mem_private: Option<u64>,
    /// Linux shared (clean+dirty).
    pub mem_shared: Option<u64>,
    /// Linux per-process swap.
    pub mem_swap: Option<u64>,
    /// Lifetime peak memory — VmHWM (peak RSS) on Linux,
    /// ri_lifetime_max_phys_footprint on macOS. The "what nearly
    /// OOM'd overnight" number.
    pub mem_peak: Option<u64>,
    /// Estimated per-process power: measured CPU-rail wattage
    /// (IOReport, macOS) apportioned by CPU share. An estimate —
    /// rendered with `~` — since true per-process energy isn't
    /// readable without sudo on any platform. None on non-macOS.
    pub power_w: Option<f32>,
}

/// Pressure Stall Information (Linux ≥4.20) — the kernel's direct
/// account of time tasks spent stalled waiting on a resource, as avg10
/// percentages. `some` = at least one task stalled; `full` = every
/// non-idle task stalled at once (severe; productivity is being lost).
/// Unlike load average or %util, PSI distinguishes "busy but fine"
/// from "actually starved".
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct PressureTick {
    pub cpu_some: f32,
    pub mem_some: f32,
    pub mem_full: f32,
    pub io_some: f32,
    pub io_full: f32,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
    /// True when per-proc net rates are connection-count estimates
    /// rather than measured counters — drives the `~` on the NET
    /// column headers.
    pub net_rates_estimated: bool,
    /// PSI stall percentages. None on non-Linux and on kernels
    /// without CONFIG_PSI.
    pub pressure: Option<PressureTick>,
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
            net_rates_estimated: true,
            pressure: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_status_labels() {
        assert_eq!(ServiceStatus::Running.label(), "Running");
        assert_eq!(ServiceStatus::Idle.label(), "Idle");
        assert_eq!(ServiceStatus::Failed.label(), "Failed");
        assert_eq!(ServiceStatus::Unknown.label(), "Unknown");
    }

    #[test]
    fn service_status_default_is_unknown() {
        // The default exists because most collectors emit Unknown when
        // the platform layer can't classify a unit; lock that in so a
        // future variant reorder doesn't silently change behavior.
        assert_eq!(ServiceStatus::default(), ServiceStatus::Unknown);
    }

    /// `.swr` recordings made before `pressure_level` existed must still
    /// replay. `--replay` is a promise about files already on disk.
    #[test]
    fn mem_tick_deserializes_without_pressure_level() {
        let old = r#"{"total_bytes":100,"used_bytes":60,"available_bytes":40,
                      "swap_total_bytes":8,"swap_used_bytes":2}"#;
        let m: MemTick = serde_json::from_str(old).expect("old MemTick must still parse");
        assert_eq!(m.total_bytes, 100);
        assert_eq!(m.pressure_level, None);
    }

    #[test]
    fn power_source_labels() {
        assert_eq!(PowerSource::Ac.label(), "AC");
        assert_eq!(PowerSource::Battery.label(), "Battery");
        assert_eq!(PowerSource::Unknown.label(), "Unknown");
    }

    #[test]
    fn power_source_default_is_unknown() {
        assert_eq!(PowerSource::default(), PowerSource::Unknown);
    }
}
