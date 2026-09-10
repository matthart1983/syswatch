//! Battery / thermal / fan collection.
//!
//! macOS (no sudo): `ioreg -rn AppleSmartBattery` for charge / cycles / health
//! / temperature / system power draw (V × A from the battery). `pmset -g batt`
//! tells us "drawing from AC" vs "Battery". `pmset -g therm` reports a CPU
//! Speed Limit when thermal throttling kicks in. Fans + per-component power
//! need `powermetrics` (sudo) — surfaced as a hint.
//!
//! Linux: `/sys/class/power_supply/BAT*/*` + `/sys/class/thermal/thermal_zone*/temp`
//! + `/sys/class/hwmon/hwmon*/fan*_input`. All readable without sudo.

use std::time::{Duration, Instant};

use crate::collect::model::*;

/// Set when our shared macOS sampler couldn't initialize at startup
/// (IOReport unavailable, SMC denied). Gives the user a hint instead of
/// silently empty fans + power readings.
#[cfg(target_os = "macos")]
const HINT_MACOS_NO_SAMPLER: &str =
    "macOS sampler unavailable — IOReport / SMC didn't initialize this run";

/// Battery / thermal data changes slowly — refreshing on every 1Hz tick would
/// spawn 3 subprocesses per second on macOS. We cache the last result and
/// re-sample at most every REFRESH interval; the UI can keep pace with the
/// fast loop without paying the subprocess tax.
const REFRESH: Duration = Duration::from_secs(5);

pub struct PowerCollector {
    last_sample_at: Option<Instant>,
    cached: PowerTick,
}

impl PowerCollector {
    pub fn new() -> Self {
        Self {
            last_sample_at: None,
            cached: PowerTick::default(),
        }
    }

    pub fn sample(
        &mut self,
        #[cfg(target_os = "macos")] macos_tick: Option<&crate::collect::macos_sampler::MacosTick>,
    ) -> PowerTick {
        let stale = self
            .last_sample_at
            .map(|t| t.elapsed() >= REFRESH)
            .unwrap_or(true);
        if stale {
            self.cached = sample_inner();
            self.last_sample_at = Some(Instant::now());
        }
        let mut tick = self.cached.clone();

        // Overlay per-tick IOReport + SMC data on macOS. system_power_w
        // and fans were previously the missing pieces of the Power tab;
        // the shared sampler in Collector now feeds them every cycle.
        #[cfg(target_os = "macos")]
        {
            match macos_tick {
                Some(m) => {
                    if let Some(w) = m.system_power_w {
                        tick.system_power_w = Some(w);
                    }
                    if !m.fans.is_empty() {
                        tick.fans = m.fans.clone();
                    }
                    tick.live_data_hint = None;
                }
                None => {
                    tick.live_data_hint = Some(HINT_MACOS_NO_SAMPLER.into());
                }
            }
        }
        tick
    }
}

#[cfg(target_os = "macos")]
fn sample_inner() -> PowerTick {
    use crate::collect::command::{run_with_timeout, PERIODIC_TIMEOUT};

    let mut tick = PowerTick::default();

    // Battery from ioreg AppleSmartBattery. We no longer derive
    // system_power_w from V·A here — the shared macOS sampler returns
    // the real per-rail total via IOReport in `PowerCollector::sample`.
    if let Some(text) = run_with_timeout("ioreg", &["-rn", "AppleSmartBattery"], PERIODIC_TIMEOUT) {
        tick.battery = parse_macos_ioreg_battery(&text);
    }

    // Power source from pmset -g batt's first line: "Now drawing from 'X Power'".
    if let Some(text) = run_with_timeout("pmset", &["-g", "batt"], PERIODIC_TIMEOUT) {
        tick.source = parse_macos_pmset_source(&text);
    }

    // Thermal throttle from pmset -g therm. If the line "CPU_Speed_Limit = N"
    // is present we use N; if pmset only prints "no warning level recorded"
    // we know the system is fine → 100.
    if let Some(text) = run_with_timeout("pmset", &["-g", "therm"], PERIODIC_TIMEOUT) {
        tick.thermal_throttle_pct = Some(parse_macos_pmset_throttle(&text));
    }

    // Hint is set by the per-tick overlay in `sample()` based on
    // whether the shared sampler initialized; leave empty here.
    tick
}

#[cfg(target_os = "linux")]
fn sample_inner() -> PowerTick {
    use std::fs;
    use std::path::Path;

    let mut tick = PowerTick::default();

    // First /sys/class/power_supply with type=Battery wins.
    if let Ok(entries) = fs::read_dir("/sys/class/power_supply") {
        for entry in entries.flatten() {
            let path = entry.path();
            let supply_type = read_trim(&path.join("type"));
            match supply_type.as_deref() {
                Some("Battery") => {
                    let bat = parse_linux_battery(&path);
                    tick.system_power_w = derive_linux_power_w(&path);
                    tick.battery = Some(bat);
                }
                Some("Mains") | Some("UPS") => {
                    if read_trim(&path.join("online")).as_deref() == Some("1") {
                        tick.source = PowerSource::Ac;
                    }
                }
                _ => {}
            }
        }
    }
    if tick.source == PowerSource::Unknown && tick.battery.is_some() {
        tick.source = PowerSource::Battery;
    }

    // Thermal zones.
    if let Ok(entries) = fs::read_dir("/sys/class/thermal") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if !name_str.starts_with("thermal_zone") {
                continue;
            }
            let path = entry.path();
            let zone_type = read_trim(&path.join("type")).unwrap_or_else(|| name_str.to_string());
            let temp_milli = read_trim(&path.join("temp"))
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or(0);
            tick.thermal_zones.push(ThermalZone {
                name: zone_type,
                temp_c: temp_milli as f32 / 1000.0,
            });
        }
    }

    // hwmon temperatures. AMD Ryzen (`k10temp`, labels Tctl/Tdie) and many
    // laptop sensors expose CPU/package temps *only* via hwmon, never as a
    // `/sys/class/thermal` zone — so an AMD box would otherwise show no CPU
    // temperature at all. Merge them in, skipping names we already have.
    for z in collect_hwmon_temps(Path::new("/sys/class/hwmon")) {
        if !tick.thermal_zones.iter().any(|e| e.name == z.name) {
            tick.thermal_zones.push(z);
        }
    }

    // Fans via hwmon (Linux exposes them per-chip, names vary).
    if let Ok(entries) = fs::read_dir("/sys/class/hwmon") {
        for entry in entries.flatten() {
            let chip = entry.path();
            for i in 1..=8 {
                let input = chip.join(format!("fan{}_input", i));
                if !Path::new(&input).exists() {
                    break;
                }
                let rpm = read_trim(&input)
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(0);
                if rpm == 0 {
                    continue;
                }
                let label = read_trim(&chip.join(format!("fan{}_label", i)))
                    .unwrap_or_else(|| format!("fan{}", i));
                let target = read_trim(&chip.join(format!("fan{}_target", i)))
                    .and_then(|s| s.parse::<u32>().ok());
                tick.fans.push(FanTick {
                    name: label,
                    rpm,
                    target_rpm: target,
                });
            }
        }
    }

    // Linux exposes throttling indirectly (cpufreq, throttle_count). Skipping
    // until we add a cpufreq collector — leave None so the UI shows "—".
    tick.thermal_throttle_pct = None;
    tick
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn sample_inner() -> PowerTick {
    PowerTick::default()
}

// ───────────────────────── parsers ─────────────────────────

#[cfg(target_os = "macos")]
fn parse_macos_ioreg_battery(text: &str) -> Option<BatteryTick> {
    let mut bat = BatteryTick::default();
    let mut saw_charge = false;
    for line in text.lines() {
        let line = line.trim();
        // Lines look like:  "FieldName" = value
        let Some(eq) = line.find(" = ") else { continue };
        let key = line[..eq].trim().trim_matches('"');
        let val = line[eq + 3..].trim();
        match key {
            "CurrentCapacity" => {
                bat.charge_pct = val.parse::<f32>().unwrap_or(0.0);
                saw_charge = true;
            }
            "MaxCapacity" => {
                // MaxCapacity in ioreg AppleSmartBattery is the *current* full
                // capacity expressed as a % of design — i.e. battery health.
                bat.health_pct = val.parse::<f32>().ok();
            }
            "CycleCount" => bat.cycle_count = val.parse().ok(),
            "Temperature" => {
                bat.temp_c = val.parse::<f32>().ok().map(|v| v / 100.0);
            }
            "Voltage" => bat.voltage_v = val.parse::<f32>().ok().map(|v| v / 1000.0),
            "Amperage" => {
                // ioreg prints this as an unsigned 64-bit int even though it's
                // semantically signed. Round-trip through u64 -> i64.
                bat.amperage_ma = val.parse::<u64>().ok().map(|v| v as i64 as i32);
            }
            "TimeRemaining" => {
                bat.time_remaining_min = val.parse::<u32>().ok().filter(|v| *v > 0 && *v < 60_000);
            }
            "IsCharging" => bat.is_charging = val.eq_ignore_ascii_case("Yes"),
            "FullyCharged" => bat.fully_charged = val.eq_ignore_ascii_case("Yes"),
            _ => {}
        }
    }
    if saw_charge {
        Some(bat)
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn parse_macos_pmset_source(text: &str) -> PowerSource {
    for line in text.lines() {
        if let Some(start) = line.find("drawing from '") {
            let rest = &line[start + "drawing from '".len()..];
            if let Some(end) = rest.find('\'') {
                let label = &rest[..end];
                if label.starts_with("AC") {
                    return PowerSource::Ac;
                }
                if label.starts_with("Battery") {
                    return PowerSource::Battery;
                }
            }
        }
    }
    PowerSource::Unknown
}

#[cfg(target_os = "macos")]
fn parse_macos_pmset_throttle(text: &str) -> u32 {
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("CPU_Speed_Limit") {
            // "CPU_Speed_Limit \t= 87"
            if let Some(eq) = rest.find('=') {
                if let Ok(n) = rest[eq + 1..].trim().parse::<u32>() {
                    return n;
                }
            }
        }
    }
    100
}

// Pure file-IO over a &Path — exercisable on any host via tempfile fixtures.
// Gated to (linux | test) so non-Linux release builds don't warn dead.
#[cfg(any(target_os = "linux", test))]
fn parse_linux_battery(path: &std::path::Path) -> BatteryTick {
    let mut bat = BatteryTick::default();
    bat.charge_pct = read_trim(&path.join("capacity"))
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(0.0);
    let status = read_trim(&path.join("status")).unwrap_or_default();
    bat.is_charging = status.eq_ignore_ascii_case("Charging");
    bat.fully_charged = status.eq_ignore_ascii_case("Full");
    bat.cycle_count = read_trim(&path.join("cycle_count")).and_then(|s| s.parse().ok());
    bat.voltage_v = read_trim(&path.join("voltage_now"))
        .and_then(|s| s.parse::<f32>().ok())
        .map(|v| v / 1_000_000.0);
    bat.amperage_ma = read_trim(&path.join("current_now"))
        .and_then(|s| s.parse::<i64>().ok())
        .map(|v| (v / 1000) as i32);
    bat.temp_c = read_trim(&path.join("temp"))
        .and_then(|s| s.parse::<f32>().ok())
        .map(|v| v / 10.0);
    let energy_full_design =
        read_trim(&path.join("energy_full_design")).and_then(|s| s.parse::<f32>().ok());
    let energy_full = read_trim(&path.join("energy_full")).and_then(|s| s.parse::<f32>().ok());
    if let (Some(d), Some(f)) = (energy_full_design, energy_full) {
        if d > 0.0 {
            bat.health_pct = Some((f / d * 100.0).clamp(0.0, 100.0));
        }
    }
    bat
}

#[cfg(any(target_os = "linux", test))]
fn derive_linux_power_w(path: &std::path::Path) -> Option<f32> {
    // power_now is in microwatts; voltage*current is the fallback.
    if let Some(uw) = read_trim(&path.join("power_now")).and_then(|s| s.parse::<f32>().ok()) {
        return Some(uw / 1_000_000.0);
    }
    let v_uv = read_trim(&path.join("voltage_now")).and_then(|s| s.parse::<f32>().ok())?;
    let c_ua = read_trim(&path.join("current_now")).and_then(|s| s.parse::<f32>().ok())?;
    Some(v_uv * c_ua.abs() / 1e12)
}

#[cfg(any(target_os = "linux", test))]
fn read_trim(p: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(p)
        .ok()
        .map(|s| s.trim().to_string())
}

/// Collect every `tempN_input` exposed by the hwmon chips under
/// `hwmon_root` (normally `/sys/class/hwmon`), as `ThermalZone`s.
///
/// This is what surfaces CPU temperature on AMD: the `k10temp` driver
/// publishes the package temperature here (labelled `Tctl`/`Tdie`) and,
/// unlike Intel's `coretemp`/`x86_pkg_temp`, registers no `thermal_zone`.
///
/// Each zone is named `"<chip> <label>"` (e.g. `"k10temp Tctl"`) when a
/// `tempN_label` is present, else `"<chip> tempN"`. Readings outside a
/// sane range are dropped (some inputs read sentinel values when idle).
/// Pure file-IO over a path so it's exercisable on any host via fixtures.
#[cfg(any(target_os = "linux", test))]
fn collect_hwmon_temps(hwmon_root: &std::path::Path) -> Vec<ThermalZone> {
    let mut zones = Vec::new();
    let Ok(entries) = std::fs::read_dir(hwmon_root) else {
        return zones;
    };
    // Stable order so the panel doesn't reshuffle between ticks.
    let mut chips: Vec<std::path::PathBuf> = entries.flatten().map(|e| e.path()).collect();
    chips.sort();

    for chip in chips {
        let chip_name = read_trim(&chip.join("name")).unwrap_or_else(|| "hwmon".into());

        // Scan the chip dir for `tempN_input` rather than guessing a range:
        // `coretemp` uses sparse, high indices (Core 32 lands at temp34), so
        // a fixed `1..=N` loop would silently drop sensors on many-core CPUs.
        let Ok(files) = std::fs::read_dir(&chip) else {
            continue;
        };
        let mut indices: Vec<u32> = files
            .flatten()
            .filter_map(|e| {
                e.file_name()
                    .to_string_lossy()
                    .strip_prefix("temp")?
                    .strip_suffix("_input")?
                    .parse::<u32>()
                    .ok()
            })
            .collect();
        indices.sort_unstable();

        for i in indices {
            let Some(raw) = read_trim(&chip.join(format!("temp{}_input", i)))
                .and_then(|s| s.parse::<i64>().ok())
            else {
                continue;
            };
            let temp_c = raw as f32 / 1000.0;
            // hwmon temps are millidegrees; ignore obviously-bogus readings
            // (disconnected sensors report values like -274°C or 0).
            if !(temp_c > -40.0 && temp_c < 150.0) {
                continue;
            }
            let name = match read_trim(&chip.join(format!("temp{}_label", i))) {
                Some(label) if !label.is_empty() => format!("{} {}", chip_name, label),
                _ => format!("{} temp{}", chip_name, i),
            };
            zones.push(ThermalZone { name, temp_c });
        }
    }
    zones
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── hwmon temperature collection (AMD k10temp & friends) ─────────────

    fn write(path: &std::path::Path, contents: &str) {
        std::fs::write(path, contents).unwrap();
    }

    /// Build a fake hwmon chip dir: `<root>/<dir>/name` + temp inputs/labels.
    fn mk_chip(root: &std::path::Path, dir: &str, name: &str, temps: &[(u32, Option<&str>, &str)]) {
        let chip = root.join(dir);
        std::fs::create_dir_all(&chip).unwrap();
        write(&chip.join("name"), name);
        for (i, label, input) in temps {
            if let Some(l) = label {
                write(&chip.join(format!("temp{}_label", i)), l);
            }
            write(&chip.join(format!("temp{}_input", i)), input);
        }
    }

    #[test]
    fn hwmon_reads_k10temp_with_labels() {
        let dir = tempfile::tempdir().unwrap();
        // AMD Ryzen: k10temp with Tctl/Tdie, in millidegrees.
        mk_chip(
            dir.path(),
            "hwmon0",
            "k10temp",
            &[(1, Some("Tctl"), "52000"), (2, Some("Tdie"), "50125")],
        );
        let zones = collect_hwmon_temps(dir.path());
        let tctl = zones.iter().find(|z| z.name == "k10temp Tctl").unwrap();
        assert_eq!(tctl.temp_c, 52.0);
        let tdie = zones.iter().find(|z| z.name == "k10temp Tdie").unwrap();
        assert_eq!(tdie.temp_c, 50.125);
    }

    #[test]
    fn hwmon_unlabeled_temp_uses_chip_and_index() {
        let dir = tempfile::tempdir().unwrap();
        mk_chip(dir.path(), "hwmon1", "amdgpu", &[(1, None, "45000")]);
        let zones = collect_hwmon_temps(dir.path());
        let edge = zones.iter().find(|z| z.name == "amdgpu temp1").unwrap();
        assert_eq!(edge.temp_c, 45.0);
    }

    #[test]
    fn hwmon_drops_bogus_readings() {
        let dir = tempfile::tempdir().unwrap();
        // -274°C (disconnected) and 999°C (sentinel) must be filtered out.
        mk_chip(
            dir.path(),
            "hwmon0",
            "nct6797",
            &[
                (1, None, "-274000"),
                (2, None, "999000"),
                (3, None, "61000"),
            ],
        );
        let zones = collect_hwmon_temps(dir.path());
        assert_eq!(zones.len(), 1);
        assert_eq!(zones[0].temp_c, 61.0);
    }

    #[test]
    fn hwmon_missing_root_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(collect_hwmon_temps(&dir.path().join("does-not-exist")).is_empty());
    }

    #[test]
    fn hwmon_reads_sparse_high_indices() {
        // Real Intel coretemp uses non-contiguous, >32 indices (Core 32 at
        // temp34). The dir scan must pick those up, not a fixed 1..=N loop.
        let dir = tempfile::tempdir().unwrap();
        mk_chip(
            dir.path(),
            "hwmon0",
            "coretemp",
            &[
                (1, Some("Package id 0"), "45000"),
                (10, Some("Core 8"), "39000"),
                (34, Some("Core 32"), "44000"),
                (35, Some("Core 33"), "44000"),
            ],
        );
        let zones = collect_hwmon_temps(dir.path());
        assert_eq!(zones.len(), 4);
        assert!(zones
            .iter()
            .any(|z| z.name == "coretemp Core 32" && z.temp_c == 44.0));
        assert!(zones.iter().any(|z| z.name == "coretemp Core 33"));
        // Sorted by index → Package (temp1) first.
        assert_eq!(zones[0].name, "coretemp Package id 0");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parses_real_ioreg_sample() {
        // Captured from a real MacBook running this branch.
        let sample = r#"
      "CurrentCapacity" = 74
      "TimeRemaining" = 378
      "Amperage" = 18446744073709551133
      "FullyCharged" = No
      "MaxCapacity" = 100
      "Temperature" = 3064
      "DesignCapacity" = 6249
      "IsCharging" = No
      "Voltage" = 12135
      "CycleCount" = 91
        "#;
        let bat = parse_macos_ioreg_battery(sample).expect("battery parsed");
        assert_eq!(bat.charge_pct as i32, 74);
        assert_eq!(bat.cycle_count, Some(91));
        assert_eq!(bat.health_pct, Some(100.0));
        assert!(!bat.is_charging);
        assert_eq!(bat.time_remaining_min, Some(378));
        assert!((bat.voltage_v.unwrap() - 12.135).abs() < 0.001);
        assert!((bat.temp_c.unwrap() - 30.64).abs() < 0.01);
        // 18446744073709551133 == -483 as i64 → -483 mA.
        assert_eq!(bat.amperage_ma, Some(-483));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parses_pmset_source_ac_and_battery() {
        let bat_sample = "Now drawing from 'Battery Power'\n -InternalBattery-0\t75%; discharging; 4:23 remaining present: true";
        assert_eq!(parse_macos_pmset_source(bat_sample), PowerSource::Battery);
        let ac_sample = "Now drawing from 'AC Power'";
        assert_eq!(parse_macos_pmset_source(ac_sample), PowerSource::Ac);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn pmset_no_throttle_returns_100() {
        let healthy = "Note: No thermal warning level has been recorded";
        assert_eq!(parse_macos_pmset_throttle(healthy), 100);
        let throttled =
            "CPU_Scheduler_Limit \t= 100\nCPU_Available_CPUs \t= 14\nCPU_Speed_Limit \t= 87";
        assert_eq!(parse_macos_pmset_throttle(throttled), 87);
    }

    // ── Linux sysfs parsers ── exercised on any host via tempfile ──────────

    fn write_field(dir: &std::path::Path, name: &str, value: &str) {
        std::fs::write(dir.join(name), value).unwrap();
    }

    #[test]
    fn linux_battery_basic_charging() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        write_field(p, "capacity", "73");
        write_field(p, "status", "Charging");
        write_field(p, "cycle_count", "127");
        write_field(p, "voltage_now", "12500000"); // 12.5 V (µV)
        write_field(p, "current_now", "1500000"); // 1.5 A (µA)
        write_field(p, "temp", "315"); // 31.5 °C (deci-C)
        write_field(p, "energy_full_design", "60000000");
        write_field(p, "energy_full", "57000000"); // 95% health

        let bat = parse_linux_battery(p);
        assert_eq!(bat.charge_pct as i32, 73);
        assert!(bat.is_charging);
        assert!(!bat.fully_charged);
        assert_eq!(bat.cycle_count, Some(127));
        assert!((bat.voltage_v.unwrap() - 12.5).abs() < 1e-3);
        assert_eq!(bat.amperage_ma, Some(1500));
        assert!((bat.temp_c.unwrap() - 31.5).abs() < 1e-3);
        assert!((bat.health_pct.unwrap() - 95.0).abs() < 1e-3);
    }

    #[test]
    fn linux_battery_full_status() {
        let dir = tempfile::tempdir().unwrap();
        write_field(dir.path(), "capacity", "100");
        write_field(dir.path(), "status", "Full");
        let bat = parse_linux_battery(dir.path());
        assert_eq!(bat.charge_pct as i32, 100);
        assert!(bat.fully_charged);
        assert!(!bat.is_charging);
    }

    #[test]
    fn linux_battery_missing_files_yield_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let bat = parse_linux_battery(dir.path());
        assert_eq!(bat.charge_pct, 0.0);
        assert!(!bat.is_charging);
        assert_eq!(bat.cycle_count, None);
        assert_eq!(bat.voltage_v, None);
        assert_eq!(bat.health_pct, None);
    }

    #[test]
    fn linux_power_w_prefers_power_now_when_present() {
        let dir = tempfile::tempdir().unwrap();
        write_field(dir.path(), "power_now", "12500000"); // 12.5 W (µW)
                                                          // Voltage/current would derive 60W — power_now wins.
        write_field(dir.path(), "voltage_now", "12000000");
        write_field(dir.path(), "current_now", "5000000");
        let w = derive_linux_power_w(dir.path()).unwrap();
        assert!((w - 12.5).abs() < 1e-3);
    }

    #[test]
    fn linux_power_w_falls_back_to_voltage_x_current() {
        let dir = tempfile::tempdir().unwrap();
        write_field(dir.path(), "voltage_now", "12000000"); // 12 V
        write_field(dir.path(), "current_now", "1500000"); // 1.5 A
        let w = derive_linux_power_w(dir.path()).unwrap();
        // 12 V * 1.5 A = 18 W
        assert!((w - 18.0).abs() < 1e-2, "expected ≈18 W, got {}", w);
    }

    #[test]
    fn linux_power_w_none_without_data() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(derive_linux_power_w(dir.path()), None);
    }

    #[test]
    fn linux_health_clamps_to_100() {
        let dir = tempfile::tempdir().unwrap();
        // Fresh battery sometimes reads as overprovisioned; clamp protects the UI.
        write_field(dir.path(), "capacity", "100");
        write_field(dir.path(), "energy_full_design", "50000");
        write_field(dir.path(), "energy_full", "55000"); // 110% raw
        let bat = parse_linux_battery(dir.path());
        assert_eq!(bat.health_pct, Some(100.0));
    }
}
