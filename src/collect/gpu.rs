//! Cross-platform GPU discovery + (where free) live util/temp/power.
//!
//! macOS: `system_profiler SPDisplaysDataType -json` (no sudo) for static
//! identity; `ioreg -r -d 1 -w 0 -c IOAccelerator` (also no sudo) for live
//! `Device Utilization %` and `In use system memory` from each accelerator's
//! `PerformanceStatistics` dict. Temperature + per-rail power still need
//! `powermetrics --samplers gpu_power` (sudo) or IOReport private FFI —
//! deferred to v0.2.
//!
//! Linux: scan `/sys/class/drm/card*/device/` for vendor/device PCI IDs and
//! read `gpu_busy_percent` per tick when the driver exposes it (AMDGPU,
//! recent i915). NVIDIA needs nvml-wrapper — feature-gated, future work.

use crate::collect::model::GpuTick;

// ── NVIDIA via nvml-wrapper (opt-in via `gpu-nvidia` feature) ────────────
//
// Lazy-initializes a single `Nvml` handle on first use. If init fails (no
// driver, library missing, container without device passthrough), every
// nvml call here returns None and the rest of the GPU pipeline falls back
// to the PCI-ID-only stub for NVIDIA cards.
#[cfg(all(target_os = "linux", feature = "gpu-nvidia"))]
mod nvidia {
    use super::GpuTick;
    use nvml_wrapper::enum_wrappers::device::TemperatureSensor;
    use nvml_wrapper::Nvml;
    use std::sync::OnceLock;

    // Option<Nvml>: None if init failed. We attempt init exactly once per
    // process; reattempts on transient failures aren't worth the complexity.
    static NVML: OnceLock<Option<Nvml>> = OnceLock::new();

    fn nvml() -> Option<&'static Nvml> {
        NVML.get_or_init(|| Nvml::init().ok()).as_ref()
    }

    pub fn discover() -> Vec<GpuTick> {
        let Some(nvml) = nvml() else {
            return Vec::new();
        };
        let count = nvml.device_count().unwrap_or(0);
        (0..count)
            .filter_map(|i| nvml.device_by_index(i).ok())
            .map(|d| {
                let mem = d.memory_info().ok();
                GpuTick {
                    name: d.name().unwrap_or_else(|_| "NVIDIA".into()),
                    vendor: "NVIDIA".into(),
                    driver: nvml.sys_driver_version().ok(),
                    vram_total_bytes: mem.as_ref().map(|m| m.total),
                    vram_used_bytes: mem.as_ref().map(|m| m.used),
                    util_pct: None,
                    renderer_util_pct: None,
                    tiler_util_pct: None,
                    temp_c: None,
                    power_w: None,
                    live_data_hint: None,
                    last_submitter_pid: None,
                }
            })
            .collect()
    }

    /// Refresh per-tick mutable fields for every NVIDIA device in `devs`.
    /// Maps NVIDIA-vendor entries to nvml indices in encounter order — same
    /// shape as the AMDGPU sysfs path.
    pub fn refresh(devs: &mut [GpuTick]) {
        let Some(nvml) = nvml() else {
            return;
        };
        let mut nv_idx: u32 = 0;
        for dev in devs.iter_mut() {
            if dev.vendor != "NVIDIA" {
                continue;
            }
            let Ok(d) = nvml.device_by_index(nv_idx) else {
                nv_idx += 1;
                continue;
            };
            if let Ok(util) = d.utilization_rates() {
                dev.util_pct = Some(util.gpu as f32);
            }
            if let Ok(mem) = d.memory_info() {
                dev.vram_total_bytes = Some(mem.total);
                dev.vram_used_bytes = Some(mem.used);
            }
            if let Ok(t) = d.temperature(TemperatureSensor::Gpu) {
                dev.temp_c = Some(t as f32);
            }
            if let Ok(mw) = d.power_usage() {
                dev.power_w = Some(mw as f32 / 1000.0);
            }
            dev.live_data_hint = None;
            nv_idx += 1;
        }
    }
}

#[cfg(target_os = "macos")]
const HINT_MACOS_NO_IOREPORT: &str =
    "IOReport unavailable — temperature + per-rail power can't be sampled";
#[cfg(target_os = "linux")]
const HINT_LINUX_NO_AMDGPU: &str =
    "amdgpu driver not loaded — load it for util/VRAM/temp/power, or use the proprietary driver";
#[cfg(target_os = "linux")]
const HINT_LINUX_NVIDIA_NO_NVML: &str =
    "build with --features nvidia (linked against libnvidia-ml) for util/VRAM/temp/power";
#[cfg(target_os = "linux")]
const HINT_LINUX_INTEL: &str =
    "i915/xe live util needs `gpu_busy_percent` (recent kernels); per-rail power not exposed";

pub struct GpuDiscovery {
    /// Cached at startup (subprocess on macOS is too slow to poll).
    pub devices: Vec<GpuTick>,
    /// Parallel to `devices`: the real DRM `cardN` index per device on Linux
    /// (`None` where there's no stable sysfs mapping, e.g. nvml NVIDIA
    /// entries). Empty on other platforms. `refresh()` reads
    /// `/sys/class/drm/cardN` live metrics by this true card number rather
    /// than the device's position in the list, which mismatches on
    /// hybrid/multi-GPU hosts where the discrete GPU isn't `card0` (issue #15).
    #[allow(dead_code)]
    card_indices: Vec<Option<u32>>,
}

impl GpuDiscovery {
    pub fn new() -> Self {
        #[cfg(target_os = "linux")]
        let (devices, card_indices) = discover_linux();
        #[cfg(not(target_os = "linux"))]
        let (devices, card_indices) = (discover(), Vec::new());
        Self {
            devices,
            card_indices,
        }
    }

    /// Refresh per-tick mutable fields. On macOS the caller passes a
    /// pre-sampled `MacosTick` (constructed by the shared sampler in the
    /// Collector); on Linux we re-read sysfs directly. The macOS arg is
    /// ignored on other platforms.
    #[allow(unused_mut, unused_variables)]
    pub fn refresh(
        &mut self,
        #[cfg(target_os = "macos")] macos_tick: Option<&crate::collect::macos_sampler::MacosTick>,
    ) -> Vec<GpuTick> {
        let mut out = self.devices.clone();

        #[cfg(target_os = "macos")]
        {
            let stats = collect_macos_gpu_stats();
            // Zip in declaration order. system_profiler and ioreg both
            // enumerate accelerators in the same order on every Apple Silicon
            // box we've seen, so positional matching is reliable.
            for (dev, s) in out.iter_mut().zip(stats.iter()) {
                dev.util_pct = Some(s.device_util_pct);
                dev.renderer_util_pct = Some(s.renderer_util_pct);
                dev.tiler_util_pct = Some(s.tiler_util_pct);
                if s.in_use_system_memory > 0 {
                    dev.vram_used_bytes = Some(s.in_use_system_memory);
                }
                dev.last_submitter_pid = s.last_submission_pid;
            }

            // IOReport+SMC values from the shared sampler. None until the
            // sampler initializes successfully (or, for power, until the
            // second tick when a delta becomes available).
            let gpu_power_w = macos_tick.and_then(|t| t.gpu_power_w);
            let gpu_temp_c = macos_tick.and_then(|t| t.gpu_temp_c);
            for dev in out.iter_mut() {
                dev.power_w = gpu_power_w;
                dev.temp_c = gpu_temp_c;
                dev.live_data_hint = match (gpu_power_w, gpu_temp_c) {
                    (Some(_), Some(_)) => None,
                    _ if macos_tick.is_some() => None, // sampler ran, data primes next tick
                    _ => Some(HINT_MACOS_NO_IOREPORT.into()),
                };
            }
        }

        #[cfg(all(target_os = "linux", feature = "gpu-nvidia"))]
        nvidia::refresh(&mut out);

        // Read live metrics by each device's *real* DRM card number, not its
        // position in the list. On hybrid/multi-GPU hosts the discrete GPU is
        // often card1 while card0 is the iGPU (or absent from the list), so a
        // positional index reads the wrong (or a nonexistent) card and live
        // telemetry comes back blank (issue #15).
        #[cfg(target_os = "linux")]
        for (dev, card_index) in out.iter_mut().zip(self.card_indices.iter()) {
            let Some(card_idx) = card_index.map(|n| n as usize) else {
                continue; // no stable sysfs card (e.g. nvml NVIDIA) — skip
            };
            // gpu_busy_percent: AMDGPU + recent i915/xe both expose it.
            if let Some(util) = read_linux_busy_percent(card_idx) {
                dev.util_pct = Some(util);
                dev.live_data_hint = None;
            }
            if dev.vendor == "AMD" {
                let device_path = amd_device_path(card_idx);
                if let Some(used) = read_amdgpu_vram_bytes(&device_path.join("mem_info_vram_used"))
                {
                    dev.vram_used_bytes = Some(used);
                }
                // hwmon dir name varies (hwmon0, hwmon1, ...) — find the
                // first one nested under device/hwmon/.
                if let Some(hwmon) = find_amdgpu_hwmon_dir(&device_path) {
                    if let Some(t) = read_hwmon_temp_c(&hwmon.join("temp1_input")) {
                        dev.temp_c = Some(t);
                    }
                    if let Some(w) = read_hwmon_power_w(&hwmon.join("power1_average")) {
                        dev.power_w = Some(w);
                    }
                }
                // We've covered util/vram/temp/power — clear the hint so
                // the UI doesn't show a "needs nvml" message on AMD.
                if dev.util_pct.is_some()
                    && dev.vram_used_bytes.is_some()
                    && (dev.temp_c.is_some() || dev.power_w.is_some())
                {
                    dev.live_data_hint = None;
                }
            }
        }

        out
    }
}

#[cfg(target_os = "macos")]
fn discover() -> Vec<GpuTick> {
    use crate::collect::command::{run_with_timeout, DISCOVERY_TIMEOUT};
    let Some(text) = run_with_timeout(
        "system_profiler",
        &["SPDisplaysDataType", "-json"],
        DISCOVERY_TIMEOUT,
    ) else {
        return Vec::new();
    };
    let Ok(parsed): Result<serde_json::Value, _> = serde_json::from_str(&text) else {
        return Vec::new();
    };
    let Some(arr) = parsed.get("SPDisplaysDataType").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .map(|d| {
            let name = d
                .get("sppci_model")
                .or_else(|| d.get("_name"))
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown GPU")
                .to_string();
            // SPDisplays returns localization keys like "sppci_vendor_Apple";
            // strip the prefix so the UI shows a real vendor name.
            let vendor = d
                .get("spdisplays_vendor")
                .and_then(|v| v.as_str())
                .map(strip_macos_vendor_key)
                .unwrap_or_else(|| "Apple".into());
            let vram = d
                .get("spdisplays_vram_shared")
                .or_else(|| d.get("spdisplays_vram"))
                .and_then(|v| v.as_str())
                .and_then(parse_vram_string);
            let driver = d
                .get("spdisplays_metalfamily")
                .or_else(|| d.get("spdisplays_mtlgpufamilysupport"))
                .and_then(|v| v.as_str())
                .map(String::from);
            GpuTick {
                name,
                vendor,
                driver,
                vram_total_bytes: vram,
                vram_used_bytes: None,
                util_pct: None,
                renderer_util_pct: None,
                tiler_util_pct: None,
                temp_c: None,
                power_w: None,
                // refresh() recomputes this on the first tick based on
                // whether the IOReport+SMC handles initialized.
                live_data_hint: None,
                last_submitter_pid: None,
            }
        })
        .collect()
}

/// One row per IOAccelerator entry from `ioreg`.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Default, PartialEq)]
struct MacGpuStats {
    device_util_pct: f32,
    renderer_util_pct: f32,
    tiler_util_pct: f32,
    in_use_system_memory: u64,
    alloc_system_memory: u64,
    /// From ioreg's `AGCInfo.fLastSubmissionPID` — the most recent
    /// process to submit GPU work. Rotating hint, not a usage metric.
    last_submission_pid: Option<u32>,
}

#[cfg(target_os = "macos")]
fn collect_macos_gpu_stats() -> Vec<MacGpuStats> {
    use crate::collect::command::{run_with_timeout, PERIODIC_TIMEOUT};
    let Some(text) = run_with_timeout(
        "ioreg",
        &["-r", "-d", "1", "-w", "0", "-c", "IOAccelerator"],
        PERIODIC_TIMEOUT,
    ) else {
        return Vec::new();
    };
    parse_ioreg_perf_stats(&text)
}

/// Parse one `MacGpuStats` per IOAccelerator block from ioreg output.
///
/// Walks the text linearly, tracking accelerator-block boundaries
/// (`+-o ` lines from `ioreg -r -c IOAccelerator`). Within a block,
/// pulls `PerformanceStatistics` (util / VRAM) and `AGCInfo`
/// (`fLastSubmissionPID`) into the same struct so the per-device
/// indices stay aligned even when a block is missing one of the keys.
#[cfg(target_os = "macos")]
fn parse_ioreg_perf_stats(text: &str) -> Vec<MacGpuStats> {
    const PERF_PREFIX: &str = "\"PerformanceStatistics\" = {";
    const AGC_PREFIX: &str = "\"AGCInfo\" = {";
    let mut out: Vec<MacGpuStats> = Vec::new();
    let mut current: Option<MacGpuStats> = None;

    for line in text.lines() {
        // Block boundary: `+-o IOAccelerator...` starts a new device.
        if line.trim_start().starts_with("+-o ") {
            if let Some(prev) = current.take() {
                out.push(prev);
            }
            current = Some(MacGpuStats::default());
            continue;
        }

        // Auto-open a block on the first content line — defensive
        // against ioreg variants that omit the `+-o` boundary, plus
        // simpler test inputs.
        if current.is_none()
            && (extract_dict_body(line, PERF_PREFIX).is_some()
                || extract_dict_body(line, AGC_PREFIX).is_some())
        {
            current = Some(MacGpuStats::default());
        }

        let Some(stats) = current.as_mut() else {
            continue;
        };

        if let Some(body) = extract_dict_body(line, PERF_PREFIX) {
            apply_perf_stats(stats, body);
        } else if let Some(body) = extract_dict_body(line, AGC_PREFIX) {
            apply_agc_info(stats, body);
        }
    }
    if let Some(last) = current.take() {
        out.push(last);
    }
    out
}

/// Returns the inside-the-braces body of `"<key>" = {...}` if `prefix`
/// matches, otherwise None.
#[cfg(target_os = "macos")]
fn extract_dict_body<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let idx = line.find(prefix)?;
    let body_start = idx + prefix.len();
    let rel_end = line[body_start..].find('}')?;
    Some(&line[body_start..body_start + rel_end])
}

#[cfg(target_os = "macos")]
fn apply_agc_info(stats: &mut MacGpuStats, body: &str) {
    for pair in body.split(',') {
        let Some(eq) = pair.find('=') else { continue };
        let key = pair[..eq].trim().trim_matches('"');
        let val = pair[eq + 1..].trim();
        if key == "fLastSubmissionPID" {
            if let Ok(p) = val.parse::<u32>() {
                stats.last_submission_pid = Some(p);
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn apply_perf_stats(stats: &mut MacGpuStats, body: &str) {
    for pair in body.split(',') {
        // Each pair is `"Key"=value`. Numbers only as values in the
        // PerformanceStatistics dict, no nested commas to worry about.
        let Some(eq) = pair.find('=') else { continue };
        let key = pair[..eq].trim().trim_matches('"');
        let val = pair[eq + 1..].trim();
        match key {
            "Device Utilization %" => {
                stats.device_util_pct = val.parse::<f32>().unwrap_or(0.0);
            }
            "Renderer Utilization %" => {
                stats.renderer_util_pct = val.parse::<f32>().unwrap_or(0.0);
            }
            "Tiler Utilization %" => {
                stats.tiler_util_pct = val.parse::<f32>().unwrap_or(0.0);
            }
            "In use system memory" => {
                stats.in_use_system_memory = val.parse::<u64>().unwrap_or(0);
            }
            "Alloc system memory" => {
                stats.alloc_system_memory = val.parse::<u64>().unwrap_or(0);
            }
            _ => {}
        }
    }
}

#[cfg(target_os = "linux")]
fn discover_linux() -> (Vec<GpuTick>, Vec<Option<u32>>) {
    use std::fs;
    let mut out = Vec::new();
    // Parallel to `out`: the real DRM `cardN` number for each device, so
    // refresh() reads live metrics from the correct card (issue #15).
    let mut card_idx: Vec<Option<u32>> = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/class/drm") else {
        return (out, card_idx);
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        // Match cardN (no suffix — skip cardN-HDMI-A-1 connector entries).
        if !name_str.starts_with("card") || name_str.contains('-') {
            continue;
        }
        // Capture the true card number (card1 != position 0 on hybrid GPUs).
        let Some(card_number) = parse_card_index(&name_str) else {
            continue;
        };
        let card_path = entry.path();
        let device_path = card_path.join("device");
        let vendor_id = fs::read_to_string(device_path.join("vendor"))
            .ok()
            .map(|s| s.trim().to_string());
        let device_id = fs::read_to_string(device_path.join("device"))
            .ok()
            .map(|s| s.trim().to_string());

        let vendor = match vendor_id.as_deref() {
            Some("0x10de") => "NVIDIA",
            Some("0x1002") => "AMD",
            Some("0x8086") => "Intel",
            _ => "Unknown",
        }
        .to_string();
        let name = format!(
            "{} {}",
            vendor,
            device_id.unwrap_or_else(|| "Unknown".into())
        );

        // Per-vendor sysfs probes. AMD: amdgpu exposes mem_info_vram_total
        // even before any client allocates. NVIDIA: PCI ID only without nvml.
        // Intel: name from device ID; util via gpu_busy_percent in refresh.
        let vram_total_bytes = if vendor == "AMD" {
            read_amdgpu_vram_bytes(&device_path.join("mem_info_vram_total"))
        } else {
            None
        };
        let live_data_hint = match vendor.as_str() {
            "AMD" => None, // refresh() will fill util/vram_used/temp/power
            "NVIDIA" => Some(HINT_LINUX_NVIDIA_NO_NVML.into()),
            "Intel" => Some(HINT_LINUX_INTEL.into()),
            _ => Some(HINT_LINUX_NO_AMDGPU.into()),
        };

        out.push(GpuTick {
            name,
            vendor,
            driver: None,
            vram_total_bytes,
            vram_used_bytes: None,
            util_pct: None,
            renderer_util_pct: None,
            tiler_util_pct: None,
            temp_c: None,
            power_w: None,
            live_data_hint,
            last_submitter_pid: None,
        });
        card_idx.push(Some(card_number));
    }
    // Replace the PCI-ID-only NVIDIA stubs with rich nvml-derived entries
    // when the feature is on and nvml init succeeds. nvml output supersedes
    // sysfs entirely for NVIDIA — no stable mapping between sysfs cardN and
    // nvml index is exposed. Drop the stubs and their card indices together so
    // `out` and `card_idx` stay aligned, then append nvml entries with no
    // sysfs card (`None`) — refresh() skips those and lets nvml drive them.
    #[cfg(feature = "gpu-nvidia")]
    {
        let nv = nvidia::discover();
        if !nv.is_empty() {
            let mut kept_dev = Vec::with_capacity(out.len());
            let mut kept_idx = Vec::with_capacity(card_idx.len());
            for (dev, idx) in out.into_iter().zip(card_idx) {
                if dev.vendor != "NVIDIA" {
                    kept_dev.push(dev);
                    kept_idx.push(idx);
                }
            }
            for dev in nv {
                kept_dev.push(dev);
                kept_idx.push(None);
            }
            out = kept_dev;
            card_idx = kept_idx;
        }
    }
    (out, card_idx)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn discover() -> Vec<GpuTick> {
    Vec::new()
}

#[cfg(target_os = "linux")]
fn read_linux_busy_percent(card_idx: usize) -> Option<f32> {
    let path = format!("/sys/class/drm/card{}/device/gpu_busy_percent", card_idx);
    let s = std::fs::read_to_string(path).ok()?;
    s.trim().parse::<f32>().ok()
}

/// Parse the DRM card number out of a `/sys/class/drm` entry name.
/// `"card0" -> 0`, `"card12" -> 12`. Connector nodes (`"card0-HDMI-A-1"`)
/// and render nodes (`"renderD128"`) return `None`.
#[cfg(any(target_os = "linux", test))]
fn parse_card_index(name: &str) -> Option<u32> {
    name.strip_prefix("card")?.parse::<u32>().ok()
}

/// sysfs device directory for a given DRM card number. Centralized so the
/// card-number → path mapping is exercised by tests (issue #15: this must
/// follow the device's real card, not its position in the discovery list).
#[cfg(any(target_os = "linux", test))]
fn amd_device_path(card_idx: usize) -> std::path::PathBuf {
    std::path::PathBuf::from(format!("/sys/class/drm/card{}/device", card_idx))
}

// ── AMDGPU sysfs helpers (linux | test for fixture-driven tests) ────────

/// Read a single u64 from `path` (whitespace-trimmed). Pure file IO so
/// tests can drive it from a tempdir.
#[cfg(any(target_os = "linux", test))]
fn read_amdgpu_vram_bytes(path: &std::path::Path) -> Option<u64> {
    std::fs::read_to_string(path)
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
}

/// AMDGPU exposes hwmon as `device/hwmon/hwmonN/`. Picks the first
/// `hwmon*` subdir; that's the AMDGPU-managed one (other hwmon
/// instances live under different parents).
#[cfg(any(target_os = "linux", test))]
fn find_amdgpu_hwmon_dir(device_path: &std::path::Path) -> Option<std::path::PathBuf> {
    let hwmon_root = device_path.join("hwmon");
    let entries = std::fs::read_dir(&hwmon_root).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with("hwmon") {
            return Some(entry.path());
        }
    }
    None
}

/// Read `temp1_input` (millicelsius) and convert to °C. amdgpu units are
/// stable across all kernels we care about.
#[cfg(any(target_os = "linux", test))]
fn read_hwmon_temp_c(path: &std::path::Path) -> Option<f32> {
    let raw: i64 = std::fs::read_to_string(path).ok()?.trim().parse().ok()?;
    Some(raw as f32 / 1000.0)
}

/// Read `power1_average` (microwatts) and convert to W. May read 0 when
/// the GPU is asleep — propagate as Some(0.0), the UI handles it.
#[cfg(any(target_os = "linux", test))]
fn read_hwmon_power_w(path: &std::path::Path) -> Option<f32> {
    let raw: u64 = std::fs::read_to_string(path).ok()?.trim().parse().ok()?;
    Some(raw as f32 / 1_000_000.0)
}

#[cfg(target_os = "macos")]
fn parse_vram_string(s: &str) -> Option<u64> {
    // "16 GB" / "8192 MB" / "1024"
    let parts: Vec<&str> = s.split_whitespace().collect();
    let n: f64 = parts.first()?.parse().ok()?;
    let mult: u64 = match parts.get(1).map(|s| s.to_ascii_uppercase()).as_deref() {
        Some("GB") => 1024 * 1024 * 1024,
        Some("MB") => 1024 * 1024,
        Some("KB") => 1024,
        _ => 1,
    };
    Some((n * mult as f64) as u64)
}

/// Strip macOS SPDisplays localization-key prefixes ("sppci_vendor_Apple" →
/// "Apple", "0x10de" → "10de"). Pulled out so it's testable without spawning
/// `system_profiler`.
#[cfg(target_os = "macos")]
fn strip_macos_vendor_key(s: &str) -> String {
    s.strip_prefix("sppci_vendor_")
        .unwrap_or(s)
        .trim_start_matches("0x")
        .to_string()
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn vram_string_handles_units() {
        assert_eq!(parse_vram_string("16 GB"), Some(16 * 1024 * 1024 * 1024));
        assert_eq!(parse_vram_string("8192 MB"), Some(8192 * 1024 * 1024));
        assert_eq!(parse_vram_string("512 KB"), Some(512 * 1024));
        // Lowercase units are folded.
        assert_eq!(parse_vram_string("4 gb"), Some(4 * 1024 * 1024 * 1024));
        // Decimals.
        assert_eq!(
            parse_vram_string("1.5 GB"),
            Some((1.5 * 1024.0 * 1024.0 * 1024.0) as u64)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn vram_string_no_unit_treated_as_bytes() {
        assert_eq!(parse_vram_string("1024"), Some(1024));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn vram_string_garbage_returns_none() {
        assert_eq!(parse_vram_string(""), None);
        assert_eq!(parse_vram_string("not a number"), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn strips_sppci_vendor_prefix() {
        assert_eq!(strip_macos_vendor_key("sppci_vendor_Apple"), "Apple");
        assert_eq!(strip_macos_vendor_key("sppci_vendor_AMD"), "AMD");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn strips_hex_vendor_id() {
        assert_eq!(strip_macos_vendor_key("0x10de"), "10de");
        assert_eq!(strip_macos_vendor_key("0x1002"), "1002");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn passes_through_unknown_format() {
        assert_eq!(strip_macos_vendor_key("Apple"), "Apple");
        assert_eq!(strip_macos_vendor_key("NVIDIA Corp"), "NVIDIA Corp");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parses_real_perf_stats_line() {
        // Captured verbatim from `ioreg -r -d 1 -w 0 -c IOAccelerator` on M3 Pro.
        let sample = r#"
+-o AGXAcceleratorG15X  <class AGXAcceleratorG15X, id 0x100000481, ...>
    {
      "model" = "Apple M3 Pro"
      "PerformanceStatistics" = {"In use system memory (driver)"=0,"Alloc system memory"=16749051904,"Tiler Utilization %"=7,"recoveryCount"=0,"lastRecoveryTime"=0,"Renderer Utilization %"=11,"TiledSceneBytes"=1441792,"Device Utilization %"=16,"SplitSceneCount"=0,"Allocated PB Size"=89915392,"In use system memory"=568164352}
    }
        "#;
        let stats = parse_ioreg_perf_stats(sample);
        assert_eq!(stats.len(), 1);
        let s = &stats[0];
        assert_eq!(s.device_util_pct as i32, 16);
        assert_eq!(s.renderer_util_pct as i32, 11);
        assert_eq!(s.tiler_util_pct as i32, 7);
        assert_eq!(s.in_use_system_memory, 568_164_352);
        assert_eq!(s.alloc_system_memory, 16_749_051_904);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn handles_multiple_accelerators() {
        let sample = r#"
+-o A
    "PerformanceStatistics" = {"Device Utilization %"=10,"In use system memory"=100}
+-o B
    "PerformanceStatistics" = {"Device Utilization %"=90,"In use system memory"=200}
        "#;
        let stats = parse_ioreg_perf_stats(sample);
        assert_eq!(stats.len(), 2);
        assert_eq!(stats[0].device_util_pct as i32, 10);
        assert_eq!(stats[1].device_util_pct as i32, 90);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn pulls_last_submission_pid_from_agc_info() {
        let sample = r#"
+-o AGXAcceleratorG15X
    "AGCInfo" = {"fLastSubmissionPID"=373,"fSubmissionsSinceLastCheck"=0,"fBusyCount"=0}
    "PerformanceStatistics" = {"Device Utilization %"=42,"In use system memory"=100}
        "#;
        let stats = parse_ioreg_perf_stats(sample);
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].last_submission_pid, Some(373));
        assert_eq!(stats[0].device_util_pct as i32, 42);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn missing_agc_info_leaves_pid_none() {
        // Block has PerfStats but no AGCInfo line.
        let sample = r#"
+-o AGX
    "PerformanceStatistics" = {"Device Utilization %"=10}
        "#;
        let stats = parse_ioreg_perf_stats(sample);
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].last_submission_pid, None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn agc_info_attribution_aligns_per_block() {
        // Only the second block has AGCInfo; ensure it lands on the
        // right device, not the first.
        let sample = r#"
+-o A
    "PerformanceStatistics" = {"Device Utilization %"=10}
+-o B
    "AGCInfo" = {"fLastSubmissionPID"=999}
    "PerformanceStatistics" = {"Device Utilization %"=90}
        "#;
        let stats = parse_ioreg_perf_stats(sample);
        assert_eq!(stats.len(), 2);
        assert_eq!(stats[0].last_submission_pid, None);
        assert_eq!(stats[1].last_submission_pid, Some(999));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn no_perf_stats_yields_empty_vec() {
        assert!(parse_ioreg_perf_stats("nothing useful here").is_empty());
        assert!(parse_ioreg_perf_stats("").is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn missing_fields_default_to_zero() {
        let sample = r#""PerformanceStatistics" = {"Device Utilization %"=42}"#;
        let stats = parse_ioreg_perf_stats(sample);
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].device_util_pct as i32, 42);
        assert_eq!(stats[0].renderer_util_pct, 0.0);
        assert_eq!(stats[0].in_use_system_memory, 0);
    }

    // ── DRM card-number mapping (issue #15) ── host-agnostic ──────────────

    use super::{amd_device_path, parse_card_index};

    #[test]
    fn parses_card_number_from_drm_entry() {
        assert_eq!(parse_card_index("card0"), Some(0));
        assert_eq!(parse_card_index("card1"), Some(1));
        assert_eq!(parse_card_index("card12"), Some(12));
    }

    #[test]
    fn rejects_connector_and_render_nodes() {
        // Connector nodes and render nodes must not be read as cards.
        assert_eq!(parse_card_index("card0-HDMI-A-1"), None);
        assert_eq!(parse_card_index("card1-DP-2"), None);
        assert_eq!(parse_card_index("renderD128"), None);
        assert_eq!(parse_card_index("card"), None);
        assert_eq!(parse_card_index("controlD64"), None);
    }

    #[test]
    fn device_path_follows_the_real_card_number() {
        // The discrete GPU on a hybrid host is card1, not the position-0
        // card0 — the live-metrics path must address card1 (issue #15).
        assert_eq!(
            amd_device_path(1),
            std::path::PathBuf::from("/sys/class/drm/card1/device")
        );
        assert_eq!(
            amd_device_path(0),
            std::path::PathBuf::from("/sys/class/drm/card0/device")
        );
    }

    // ── AMDGPU sysfs helpers ── exercised on any host via tempfile ────────

    use super::{
        find_amdgpu_hwmon_dir, read_amdgpu_vram_bytes, read_hwmon_power_w, read_hwmon_temp_c,
    };
    use std::fs;

    #[test]
    fn amdgpu_vram_parses_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mem_info_vram_total");
        fs::write(&path, "17163091968\n").unwrap();
        assert_eq!(read_amdgpu_vram_bytes(&path), Some(17_163_091_968));
    }

    #[test]
    fn amdgpu_vram_missing_file_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_amdgpu_vram_bytes(&dir.path().join("missing")), None);
    }

    #[test]
    fn amdgpu_vram_garbage_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mem_info_vram_total");
        fs::write(&path, "not a number").unwrap();
        assert_eq!(read_amdgpu_vram_bytes(&path), None);
    }

    #[test]
    fn finds_first_hwmon_subdir() {
        let dir = tempfile::tempdir().unwrap();
        let hwmon0 = dir.path().join("hwmon").join("hwmon3");
        fs::create_dir_all(&hwmon0).unwrap();
        let found = find_amdgpu_hwmon_dir(dir.path()).unwrap();
        assert_eq!(found, hwmon0);
    }

    #[test]
    fn skips_non_hwmon_entries_in_hwmon_dir() {
        let dir = tempfile::tempdir().unwrap();
        // Some kernels stash a `subsystem` symlink in here too.
        fs::create_dir_all(dir.path().join("hwmon").join("hwmon2")).unwrap();
        let found = find_amdgpu_hwmon_dir(dir.path()).unwrap();
        assert!(found
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("hwmon"));
    }

    #[test]
    fn no_hwmon_dir_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(find_amdgpu_hwmon_dir(dir.path()), None);
    }

    #[test]
    fn hwmon_temp_converts_millicelsius() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("temp1_input");
        fs::write(&path, "65500\n").unwrap();
        assert_eq!(read_hwmon_temp_c(&path), Some(65.5));
    }

    #[test]
    fn hwmon_power_converts_microwatts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("power1_average");
        fs::write(&path, "85000000\n").unwrap();
        assert_eq!(read_hwmon_power_w(&path), Some(85.0));
    }

    #[test]
    fn hwmon_power_zero_passes_through() {
        // GPU asleep — driver returns 0 µW, not "missing".
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("power1_average");
        fs::write(&path, "0\n").unwrap();
        assert_eq!(read_hwmon_power_w(&path), Some(0.0));
    }
}
