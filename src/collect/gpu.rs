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

#[cfg(target_os = "macos")]
const HINT_MACOS_TEMP_POWER: &str =
    "temperature + per-rail power need `sudo powermetrics --samplers gpu_power` (deferred)";
#[cfg(target_os = "linux")]
const HINT_LINUX_GENERIC: &str =
    "driver doesn't expose gpu_busy_percent; install nvml or amdgpu-tools";

pub struct GpuDiscovery {
    /// Cached at startup (subprocess on macOS is too slow to poll).
    pub devices: Vec<GpuTick>,
}

impl GpuDiscovery {
    pub fn new() -> Self {
        Self {
            devices: discover(),
        }
    }

    /// Refresh per-tick mutable fields. On macOS we shell out to `ioreg` and
    /// pull live `PerformanceStatistics` from each IOAccelerator. On Linux we
    /// re-read `gpu_busy_percent` from sysfs.
    #[allow(unused_mut, unused_variables)]
    pub fn refresh(&mut self) -> Vec<GpuTick> {
        let mut out = self.devices.clone();

        #[cfg(target_os = "macos")]
        {
            let stats = collect_macos_gpu_stats();
            // Zip in declaration order. system_profiler and ioreg both
            // enumerate accelerators in the same order on every Apple Silicon
            // box we've seen, so positional matching is reliable.
            for (dev, s) in out.iter_mut().zip(stats.iter()) {
                dev.util_pct = Some(s.device_util_pct);
                if s.in_use_system_memory > 0 {
                    dev.vram_used_bytes = Some(s.in_use_system_memory);
                }
                // Live util is here; keep the hint focused on what's still
                // missing (temp + power).
                dev.live_data_hint = Some(HINT_MACOS_TEMP_POWER.into());
            }
        }

        #[cfg(target_os = "linux")]
        for (i, dev) in out.iter_mut().enumerate() {
            if let Some(util) = read_linux_busy_percent(i) {
                dev.util_pct = Some(util);
                dev.live_data_hint = None;
            }
        }

        out
    }
}

#[cfg(target_os = "macos")]
fn discover() -> Vec<GpuTick> {
    use std::process::Command;
    let output = Command::new("system_profiler")
        .args(["SPDisplaysDataType", "-json"])
        .output();
    let Ok(out) = output else { return Vec::new() };
    let text = String::from_utf8_lossy(&out.stdout);
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
                temp_c: None,
                power_w: None,
                live_data_hint: Some(HINT_MACOS_TEMP_POWER.into()),
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
}

#[cfg(target_os = "macos")]
fn collect_macos_gpu_stats() -> Vec<MacGpuStats> {
    use std::process::Command;
    let Ok(out) = Command::new("ioreg")
        .args(["-r", "-d", "1", "-w", "0", "-c", "IOAccelerator"])
        .output()
    else {
        return Vec::new();
    };
    parse_ioreg_perf_stats(&String::from_utf8_lossy(&out.stdout))
}

/// Parse `"PerformanceStatistics" = {"key"=val,...}` lines out of ioreg
/// output. One entry per accelerator. Pure string work — exercised by tests.
#[cfg(target_os = "macos")]
fn parse_ioreg_perf_stats(text: &str) -> Vec<MacGpuStats> {
    const PREFIX: &str = "\"PerformanceStatistics\" = {";
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(idx) = line.find(PREFIX) else {
            continue;
        };
        let body_start = idx + PREFIX.len();
        let Some(rel_end) = line[body_start..].find('}') else {
            continue;
        };
        let body = &line[body_start..body_start + rel_end];

        let mut stats = MacGpuStats::default();
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
        out.push(stats);
    }
    out
}

#[cfg(target_os = "linux")]
fn discover() -> Vec<GpuTick> {
    use std::fs;
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/class/drm") else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        // Match cardN (no suffix).
        if !name_str.starts_with("card") || name_str.contains('-') {
            continue;
        }
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

        out.push(GpuTick {
            name,
            vendor,
            driver: None,
            vram_total_bytes: None,
            vram_used_bytes: None,
            util_pct: None,
            temp_c: None,
            power_w: None,
            live_data_hint: Some(HINT_LINUX_GENERIC.into()),
        });
    }
    out
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
}
