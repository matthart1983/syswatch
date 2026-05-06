//! Shared macOS IOReport + SMC sampler.
//!
//! macpow's `IOReportSampler` and `SmcConnection` are stateful: IOReport
//! needs two consecutive samples to derive power (energy/dt), and SMC
//! caches per-key info to avoid re-querying the controller. Both pieces
//! are expensive to spin up and modest-but-not-free to call per tick, so
//! this module owns one of each and exposes a single `tick()` method
//! that the rest of the crate consumes.
//!
//! macpow types are deliberately not re-exported. Each tick returns a
//! [`MacosTick`] typed in syswatch's own data shapes — that way swapping
//! the sampler implementation later (direct IOReport FFI, or a different
//! crate) doesn't ripple into `gpu.rs` and `power.rs`.

#![cfg(target_os = "macos")]

use crate::collect::model::FanTick;

/// One tick of macOS-only platform data, post-translated to syswatch
/// types so callers don't see macpow.
#[derive(Debug, Clone, Default)]
pub struct MacosTick {
    /// Per-rail GPU power (W). None on tick 0 (no previous sample yet)
    /// or whenever the IOReport delta couldn't be computed.
    pub gpu_power_w: Option<f32>,
    /// Hottest GPU thermistor (Tg* SMC keys, °C). None when no sensor
    /// reports a fresh, in-range value this tick.
    pub gpu_temp_c: Option<f32>,
    /// Total SoC power across every rail IOReport reports (W).
    pub system_power_w: Option<f32>,
    /// Aggregate CPU power, P-cluster + E-cluster + caches (W).
    pub cpu_power_w: Option<f32>,
    /// Apple Neural Engine power (W). Useful as a "is ML running?" hint.
    pub ane_power_w: Option<f32>,
    /// Fan readings, mapped from SMC into our FanTick shape.
    pub fans: Vec<FanTick>,
}

pub struct MacosSampler {
    sampler: macpow::ioreport::IOReportSampler,
    smc: macpow::smc::SmcConnection,
    prev_sample: Option<macpow::ioreport::Sample>,
}

impl MacosSampler {
    /// Initialize both subsystems. Any failure during construction
    /// returns None — callers fall back to whatever the platform
    /// gave them previously.
    pub fn try_init() -> Option<Self> {
        let sampler = macpow::ioreport::IOReportSampler::new().ok()?;
        let mut smc = macpow::smc::SmcConnection::open().ok()?;
        // SMC needs a one-time async key-discovery phase. Drive it
        // synchronously here — the cost is small (~ms) and synchronous
        // keeps initialization contained inside this constructor.
        let handle = smc.start_temp_discovery();
        smc.finish_temp_discovery(handle);
        Some(Self {
            sampler,
            smc,
            prev_sample: None,
        })
    }

    /// Take one IOReport + SMC sample and project it into a `MacosTick`.
    /// Each sub-step is independently fallible; any single failure
    /// leaves that field as None and the others still populate.
    pub fn tick(&mut self) -> MacosTick {
        let mut out = MacosTick::default();

        if let Ok(cur) = self.sampler.sample() {
            if let Some(prev) = self.prev_sample.as_ref() {
                if let Ok(power) = self.sampler.parse_power(prev, &cur) {
                    out.gpu_power_w = Some(power.gpu_w);
                    out.cpu_power_w = Some(power.cpu_w);
                    out.ane_power_w = Some(power.ane_w);
                    out.system_power_w = Some(power.total_w);
                }
            }
            self.prev_sample = Some(cur);
        }

        // Hottest fresh GPU thermistor. macOS reports several Tg* sensors
        // (die / package / proximity); the hottest is the headline.
        let temps = self.smc.read_temperatures();
        out.gpu_temp_c = temps
            .iter()
            .filter(|t| t.category == "GPU" && !t.stale)
            .map(|t| t.value_celsius)
            .fold(None, |acc, v| Some(acc.map_or(v, |a: f32| a.max(v))));

        // Fans: macpow returns actual + min/max — syswatch's FanTick
        // surfaces actual RPM and the platform-reported max as the
        // "target" (closest analogue when no real target is published).
        out.fans = self
            .smc
            .read_fans()
            .into_iter()
            .map(|f| FanTick {
                name: if f.name.is_empty() {
                    format!("fan{}", f.id)
                } else {
                    f.name
                },
                rpm: f.actual_rpm.max(0.0) as u32,
                target_rpm: if f.max_rpm > 0.0 {
                    Some(f.max_rpm as u32)
                } else {
                    None
                },
            })
            .collect();

        out
    }
}
