//! SysWatch Lite — the minimal single-screen view.
//!
//! One screen answering one question: *why is my machine hot, slow, or loud?*
//! Drawn at an 80×24 reference grid with six advertised keys and four colors,
//! it is the deliberate counterpart to the twelve-tab full TUI — a different
//! product for someone with one machine, not the full tool with tabs hidden.
//!
//! It is also the sibling of NetWatch Lite: identical grid geometry, band
//! rows, table column positions, keys and palette, so muscle memory carries
//! between them. Only the subject changes (CPU/memory instead of down/up,
//! processes instead of talkers). **Preserve that symmetry** — the column
//! constants below are shared with `netwatch`'s `src/ui/lite.rs` and the
//! reference-size test locks them in place.
//!
//! Everything positional lives in this file as a `const`, so a layout change
//! is a one-line edit rather than a hunt through render code — and the tests
//! at the bottom assert the fields tile without overlapping and end exactly on
//! the content edge.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::app::{App, History};
use crate::collect::{ProcTick, Snapshot};
use crate::ui::graph;
use crate::ui::palette as p;

// ── The reference grid ──────────────────────────────────────────────────────
//
// 80×24 is the size the design was drawn at and the size this layout must
// reproduce character-for-character. It is *not* a cap: `Layout` below
// generalises these to whatever terminal is actually attached.

pub const GRID_W: u16 = 80;
pub const GRID_H: u16 = 24;

/// Content starts at col 1 — col 0 and col 79 are padding.
pub const CONTENT_X: u16 = 1;
/// Content width in columns at the reference size.
pub const CONTENT_W: u16 = 78;

pub const ROW_HEADER: u16 = 0;
pub const ROW_CPU_LABEL: u16 = 2;
pub const ROW_CPU_CHART: u16 = 3;
pub const CPU_CHART_H: u16 = 3;
pub const ROW_MEM_LABEL: u16 = 6;
pub const ROW_MEM_CHART: u16 = 7;
pub const MEM_CHART_H: u16 = 2;
pub const ROW_AXIS: u16 = 9;
pub const ROW_VITALS: u16 = 10;
pub const ROW_TABLE_HEAD: u16 = 12;
pub const ROW_RULE: u16 = 13;
pub const ROW_PROCS: u16 = 14;
/// Process rows at the reference size with no detail block open.
pub const PROC_ROWS: u16 = 8;
pub const ROW_PROMPT: u16 = 22;
pub const ROW_FOOTER: u16 = 23;

/// Rows the detail block costs off the bottom of the process list.
pub const DETAIL_ROWS: u16 = 3;

// ── Grid invariants, checked at compile time ────────────────────────────────
//
// These are the constraints that make the reference grid a grid rather than a
// pile of numbers. A `const` assertion means breaking one is a build error at
// the line you edited, not a visual glitch someone notices later.

/// CPU is three rows tall and memory two. The asymmetry is a claim, not an
/// oversight: when a machine feels wrong, CPU is the answer more often than
/// RAM. Do not equalize them.
const _: () = assert!(CPU_CHART_H > MEM_CHART_H);
/// The charts and the axis must not collide.
const _: () = assert!(ROW_CPU_CHART + CPU_CHART_H == ROW_MEM_LABEL);
const _: () = assert!(ROW_MEM_CHART + MEM_CHART_H == ROW_AXIS);
/// The process list runs from its first row to the prompt, and the footer is
/// the last row of the grid.
const _: () = assert!(ROW_PROCS + PROC_ROWS == ROW_PROMPT);
const _: () = assert!(ROW_PROMPT + 1 == ROW_FOOTER);
const _: () = assert!(ROW_FOOTER == GRID_H - 1);
/// The detail block has to fit in the list it displaces.
const _: () = assert!(DETAIL_ROWS < PROC_ROWS);

/// Per-row sparkline width. Each column is a *bucket max* over the history,
/// so a spike anywhere inside a bucket survives the downsample.
pub const SPARK_W: u16 = 9;

// ── Headline number geometry ────────────────────────────────────────────────
//
// The two bright numbers are the only things on their rows that change width,
// and their units sit at absolute columns behind them. So they are formatted
// to a fixed width and right-aligned — a percentage that reflows `% cpu` once
// a second is the single most distracting thing this screen could do.

/// CPU percent ends here (width 3: `  7`, ` 24`, `100`).
pub const CPU_VAL_X_END: u16 = 5;
/// `% cpu` starts here, and never moves.
pub const CPU_UNIT_X: u16 = 7;
/// Memory used-GB ends here (width 4: ` 4.2`, `19.8`, ` 128`).
pub const MEM_VAL_X_END: u16 = 6;
/// `GB / <total>` starts here, and never moves.
pub const MEM_UNIT_X: u16 = 8;

/// A headline number must never reach its unit — the whole reason both are
/// fixed-width and fixed-position.
const _: () = assert!(CPU_VAL_X_END < CPU_UNIT_X);
const _: () = assert!(MEM_VAL_X_END < MEM_UNIT_X);

// ── Vitals row ──────────────────────────────────────────────────────────────

/// One `label value` pair on the vitals line.
///
/// Fixed columns rather than pairs flowed with three spaces between them:
/// vitals change width constantly in real use (`fan 900rpm` → `fan 1800rpm`),
/// and a flowed row shifts every label to the right of the one that changed —
/// worst of all exactly when the machine goes bad. The alert state promises no
/// layout shift; this is what makes that true.
pub struct Vital {
    pub label: &'static str,
    pub label_x: u16,
    pub val_x: u16,
    /// Value field width. Sized to the widest plausible sample.
    pub w: u16,
}

pub const VITALS: &[Vital] = &[
    Vital {
        label: "temp",
        label_x: 1,
        val_x: 6,
        w: 5, // '100°C'
    },
    Vital {
        label: "fan",
        label_x: 14,
        val_x: 18,
        w: 7, // '4800rpm'
    },
    Vital {
        label: "power",
        label_x: 28,
        val_x: 34,
        w: 4, // '100W'
    },
    Vital {
        label: "disk",
        label_x: 41,
        val_x: 46,
        w: 8, // '999 MB/s' — fmt_rate is capped at 8 columns
    },
];

// ── Table columns ───────────────────────────────────────────────────────────

/// One column of the process table at the reference size.
pub struct Field {
    pub header: &'static str,
    pub x: u16,
    pub w: u16,
}

impl Field {
    /// Last column, inclusive.
    pub const fn x_end(&self) -> u16 {
        self.x + self.w - 1
    }
}

/// Process table columns at the reference size. Verified by test to tile
/// without overlap and end exactly on the content edge — and identical to
/// NetWatch Lite's talker table, which is the whole point of the family.
///
/// `Layout` generalises these to the real terminal; the headers are read
/// straight out of here so the two can't disagree about what a column is
/// called.
pub const FIELDS: &[Field] = &[
    Field {
        header: "PROCESS",
        x: 1,
        w: 15,
    },
    Field {
        header: "USER",
        x: 17,
        w: 22,
    },
    Field {
        header: "CPU",
        x: 40,
        w: 10,
    },
    Field {
        header: "MEM",
        x: 51,
        w: 10,
    },
    Field {
        header: "THR",
        x: 62,
        w: 7,
    },
    // The header is a placeholder: the real one is computed from the tick,
    // because the span this sparkline covers depends on it. See
    // `spark_header`. Kept the same display width as the widest computed
    // value so the const layout assertions still describe the real column.
    Field {
        header: "···",
        x: 70,
        w: SPARK_W,
    },
];

/// The `60s`-style label over the per-process sparkline.
///
/// Derived rather than fixed: the sparkline is exactly
/// [`crate::app::PROC_CPU_SPARK_LEN`] samples deep, so the wall-clock span it
/// covers is that times the tick. It shipped as a hardcoded `60s`, which was
/// wrong at every tick — at the 1 Hz default the eight samples are eight
/// seconds, not sixty. A column that misreports its own time base is worse
/// than one with no label at all.
pub fn spark_header(tick_ms: u64) -> String {
    let secs = (crate::app::PROC_CPU_SPARK_LEN as u64).saturating_mul(tick_ms.max(1)) / 1000;
    if secs >= 120 {
        format!("{}m", secs / 60)
    } else {
        format!("{}s", secs.max(1))
    }
}

/// Index into [`FIELDS`], so render code names a column rather than counting.
pub const F_PROCESS: usize = 0;
pub const F_USER: usize = 1;
pub const F_CPU: usize = 2;
pub const F_MEM: usize = 3;
pub const F_THR: usize = 4;
pub const F_SPARK: usize = 5;

/// The sparkline is the last column and ends exactly on the content edge —
/// the invariant every right-anchored field in `Layout` is derived from.
const _: () = assert!(FIELDS[F_SPARK].x_end() == CONTENT_X + CONTENT_W - 1);
/// CPU and MEM are the same width; the `w_rate` in render code assumes it.
const _: () = assert!(FIELDS[F_CPU].w == FIELDS[F_MEM].w);

/// Widths of the fixed-width fields, and the single blank column between them.
const W_PROCESS: u16 = 15;
const W_RATE: u16 = 10;
const W_THR: u16 = 7;
const FIELD_GAP: u16 = 1;

// ── Footer ──────────────────────────────────────────────────────────────────

/// Keys advertised in the footer.
///
/// Navigation (`↑`/`↓`/`j`/`k`) and `Esc` are deliberately absent: they are
/// conventions from `less`/`vim`/`top` and live in the `?` overlay instead.
/// The design handoff claimed "five keybindings" while omitting any way to
/// move the selection — but the screen has a selected row, a selection tint
/// and a `↵ detail` action, all meaningless if the selection is fixed. This
/// is the honest set, and it matches NetWatch Lite exactly.
pub const FOOTER_KEYS: &[(&str, &str)] = &[
    ("q", "quit"),
    ("p", "pause"),
    ("/", "filter"),
    ("↵", "detail"),
    ("L", "full"),
    ("?", "help"),
];

/// Blank columns between footer key pairs.
pub const FOOTER_GAP: u16 = 3;

/// Right-aligned footer version string.
///
/// Lite is a *mode* of syswatch, not a second binary, so this is the syswatch
/// version rather than a hand-maintained `syswatch-lite 0.1.0`.
pub fn footer_version() -> String {
    format!("syswatch {}", env!("CARGO_PKG_VERSION"))
}

/// Rendered width of the footer key list, in columns.
pub fn footer_keys_width() -> u16 {
    let pairs: u16 = FOOTER_KEYS
        .iter()
        .map(|(k, label)| (k.width() + 1 + label.width()) as u16)
        .sum();
    pairs + FOOTER_GAP * (FOOTER_KEYS.len() as u16 - 1)
}

// ── Adaptive layout ─────────────────────────────────────────────────────────

/// Resolved geometry for the terminal Lite is actually running in.
///
/// The `FIELDS` constants describe the 80×24 reference grid; this generalises
/// them so Lite is a usable mode at any size rather than an 80-column postage
/// stamp in the corner of a wide terminal. USER absorbs surplus width (it is
/// the field most often truncated, exactly as HOST is in NetWatch Lite) and
/// the process list absorbs surplus height.
///
/// At exactly 80×24 this reproduces `FIELDS` and the row constants
/// character-for-character — locked by `layout_at_reference_size_matches_spec`.
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    pub content_x: u16,
    pub content_w: u16,
    pub x_process: u16,
    pub w_process: u16,
    pub x_user: u16,
    pub w_user: u16,
    pub x_cpu: u16,
    pub x_mem: u16,
    pub x_thr: u16,
    pub x_spark: u16,
    pub row_procs: u16,
    /// Process rows available with no detail block open.
    pub proc_rows: u16,
    pub row_prompt: u16,
    pub row_footer: u16,
}

impl Layout {
    /// Resolve the layout for `area`.
    ///
    /// Saturating throughout, because this is called with sizes that can't
    /// actually render: the key handler needs a layout to clamp scrolling
    /// against, and it runs before the first frame has told anyone how big
    /// the terminal is. A degenerate `Rect` must produce a nonsense layout,
    /// not a panic — `render` is what refuses to draw below the reference
    /// size, and it checks before it gets here.
    pub fn new(area: Rect) -> Self {
        let content_x = area.x + 1;
        let content_w = area.width.saturating_sub(2);
        let x_end = content_x + content_w.saturating_sub(1);

        // Right-anchored fields, walking leftward from the content edge.
        let x_spark = (x_end + 1).saturating_sub(SPARK_W);
        let x_thr = x_spark.saturating_sub(FIELD_GAP + W_THR);
        let x_mem = x_thr.saturating_sub(FIELD_GAP + W_RATE);
        let x_cpu = x_mem.saturating_sub(FIELD_GAP + W_RATE);

        // Left-anchored, with USER taking whatever is left in the middle.
        let x_process = content_x;
        let x_user = x_process + W_PROCESS + FIELD_GAP;
        let w_user = x_cpu.saturating_sub(FIELD_GAP).saturating_sub(x_user);

        let row_footer = (area.y + area.height).saturating_sub(1);
        let row_prompt = row_footer.saturating_sub(1);
        let row_procs = area.y + ROW_PROCS;

        Self {
            content_x,
            content_w,
            x_process,
            w_process: W_PROCESS,
            x_user,
            w_user,
            x_cpu,
            x_mem,
            x_thr,
            x_spark,
            row_procs,
            proc_rows: row_prompt.saturating_sub(row_procs),
            row_prompt,
            row_footer,
        }
    }

    pub fn content_x_end(&self) -> u16 {
        self.content_x + self.content_w - 1
    }

    /// Process rows available given whether the detail block is open.
    pub fn visible_procs(&self, detail_open: bool) -> u16 {
        if detail_open {
            self.proc_rows.saturating_sub(DETAIL_ROWS)
        } else {
            self.proc_rows
        }
    }
}

// ── Alerts ──────────────────────────────────────────────────────────────────
//
// The Insights anomaly engine is an explicit non-goal for Lite, so red is
// driven by a small table of fixed thresholds instead. Red is never
// decorative: if it is on screen, something is actually wrong — which is only
// believable if it also never flaps, hence the hysteresis.

/// Consecutive samples a condition must hold before it fires.
pub const ALERT_FIRE_SAMPLES: u8 = 3;
/// Consecutive samples below threshold before an alert clears.
pub const ALERT_CLEAR_SAMPLES: u8 = 5;

/// CPU package temperature at or above which the thermal alert fires.
pub const TEMP_ALERT_C: f32 = 95.0;
/// CPU package temperature at or above which the `temp` vital renders red.
pub const TEMP_WARN_C: f32 = 90.0;
/// Fan RPM at or above which the `fan` vital renders red. A fallback: no
/// platform we collect from reports a reliable maximum RPM, so this is a
/// "screaming" threshold rather than a percentage of spec.
pub const FAN_WARN_RPM: u32 = 4500;
/// Swap churn (bytes/sec) at or above which the swap alert fires.
pub const SWAP_ALERT_BPS: f64 = 10.0 * 1024.0 * 1024.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Alert {
    #[default]
    None,
    Thermal,
    /// Actively paging: swap is churning right now.
    SwapThrash,
    /// Out of headroom: the kernel reports critical memory pressure, or a
    /// large swap file sits under a machine with almost no RAM left.
    MemoryPressure,
}

impl Alert {
    /// The right-aligned verdict on the vitals row.
    ///
    /// Paging and starvation get separate verdicts because they are separate
    /// problems with separate fixes, and because a machine holding a big swap
    /// file while paging nothing is not "thrashing" — calling it that is how
    /// a user learns to disbelieve red.
    pub fn verdict(self) -> &'static str {
        match self {
            Alert::None => "all nominal",
            Alert::Thermal => "cpu thermal throttling",
            Alert::SwapThrash => "swap thrashing",
            Alert::MemoryPressure => "memory pressure critical",
        }
    }

    pub fn is_alert(self) -> bool {
        !matches!(self, Alert::None)
    }
}

/// Debounce counter for one condition.
#[derive(Debug, Clone, Copy, Default)]
struct Hysteresis {
    above: u8,
    below: u8,
    active: bool,
}

impl Hysteresis {
    fn update(&mut self, condition: bool) -> bool {
        if condition {
            self.below = 0;
            self.above = self.above.saturating_add(1);
            if self.above >= ALERT_FIRE_SAMPLES {
                self.active = true;
            }
        } else {
            self.above = 0;
            self.below = self.below.saturating_add(1);
            if self.below >= ALERT_CLEAR_SAMPLES {
                self.active = false;
            }
        }
        self.active
    }
}

/// Per-condition debounce state, advanced once per collected sample.
///
/// Lives on `App` rather than being recomputed at render time: render runs
/// many times per tick, and counting frames instead of samples would make the
/// debounce window depend on how fast the terminal redraws.
#[derive(Debug, Clone, Copy, Default)]
pub struct AlertTracker {
    thermal: Hysteresis,
    swap: Hysteresis,
    memory: Hysteresis,
    active: Alert,
}

impl AlertTracker {
    /// Advance one sample. Call once per collected snapshot, not per frame.
    pub fn update(&mut self, snap: &Snapshot, swap_rate: Option<f64>) {
        // An unavailable sensor is not trouble — it can never fire an alert.
        let throttling = snap.power.thermal_throttle_pct.is_some_and(|pct| pct < 100);
        let hot = cpu_temp_c(snap).is_some_and(|c| c >= TEMP_ALERT_C);
        let thermal = self.thermal.update(throttling || hot);

        let swap = self
            .swap
            .update(swap_rate.is_some_and(|r| r >= SWAP_ALERT_BPS));
        let memory = self.memory.update(mem_pressure(snap) == Pressure::Critical);

        // One verdict, always. Thermal outranks the memory conditions: it is
        // the one that damages hardware and throttles everything else. Active
        // paging outranks mere starvation because it is the acute form.
        self.active = if thermal {
            Alert::Thermal
        } else if swap {
            Alert::SwapThrash
        } else if memory {
            Alert::MemoryPressure
        } else {
            Alert::None
        };
    }

    pub fn active(&self) -> Alert {
        self.active
    }
}

// ── Reading the snapshot ────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pressure {
    None,
    Warning,
    Critical,
}

impl Pressure {
    pub fn label(self) -> &'static str {
        match self {
            Pressure::None => "none",
            Pressure::Warning => "warning",
            Pressure::Critical => "critical",
        }
    }
}

/// Memory pressure state for the memory label.
///
/// Three sources, best first. Linux has PSI — the kernel's own account of time
/// spent stalled on memory. macOS has `kern.memorystatus_vm_pressure_level`,
/// the same authority in three-state form. Only when neither is readable do we
/// infer from swap and headroom.
///
/// The inference is the weak one on purpose, and must never outrank a kernel
/// that is willing to answer: macOS holds memory full and keeps a swap file as
/// its *normal* operating state, so "swapping at all, with little available"
/// describes a healthy Mac as readily as a struggling one. Firing red on that
/// is how a user learns to disbelieve red.
pub fn mem_pressure(snap: &Snapshot) -> Pressure {
    if let Some(psi) = snap.pressure {
        return if psi.mem_full >= 10.0 {
            Pressure::Critical
        } else if psi.mem_some >= 10.0 {
            Pressure::Warning
        } else {
            Pressure::None
        };
    }
    if let Some(level) = snap.mem.pressure_level {
        return match level {
            crate::collect::MemPressureLevel::Critical => Pressure::Critical,
            crate::collect::MemPressureLevel::Warning => Pressure::Warning,
            crate::collect::MemPressureLevel::Normal => Pressure::None,
        };
    }
    let total = snap.mem.total_bytes;
    if total == 0 {
        return Pressure::None;
    }
    let headroom = snap.mem.available_bytes as f64 / total as f64;
    let swapping = snap.mem.swap_used_bytes > 0;
    if swapping && headroom < 0.10 {
        Pressure::Critical
    } else if swapping || headroom < 0.20 {
        Pressure::Warning
    } else {
        Pressure::None
    }
}

/// CPU package temperature, preferring a zone that names itself as the CPU
/// and falling back to the hottest zone reported.
pub fn cpu_temp_c(snap: &Snapshot) -> Option<f32> {
    let named = snap.power.thermal_zones.iter().find(|z| {
        let n = z.name.to_ascii_lowercase();
        n.contains("cpu") || n.contains("package") || n.contains("soc") || n.contains("die")
    });
    named
        .map(|z| z.temp_c)
        .or_else(|| {
            snap.power
                .thermal_zones
                .iter()
                .map(|z| z.temp_c)
                .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        })
        .filter(|c| *c > 0.0)
}

/// Fastest fan currently spinning. One number for a machine that may have
/// several — the question Lite answers is "are the fans screaming?", not
/// "which fan".
pub fn fan_rpm(snap: &Snapshot) -> Option<u32> {
    snap.power
        .fans
        .iter()
        .map(|f| f.rpm)
        .max()
        .filter(|r| *r > 0)
}

pub fn power_w(snap: &Snapshot) -> Option<f32> {
    snap.power.system_power_w.filter(|w| *w > 0.0)
}

pub fn disk_rate(snap: &Snapshot) -> f64 {
    snap.disk_io.read_rate + snap.disk_io.write_rate
}

/// Swap churn in bytes/sec, from the last two swap samples.
///
/// syswatch collects swap *level*, not the kernel's in/out counters, so this
/// is the absolute change in swap used per second — it catches a machine
/// actively paging in either direction, which is what "thrashing" means to a
/// user, without claiming to be a true in+out rate.
pub fn swap_rate(history: &History, tick_ms: u64) -> Option<f64> {
    let n = history.swap.len();
    if n < 2 || tick_ms == 0 {
        return None;
    }
    let last = *history.swap.nth_back(0)?;
    let prev = *history.swap.nth_back(1)?;
    let delta = last.abs_diff(prev) as f64;
    Some(delta / (tick_ms as f64 / 1000.0))
}

/// One row of the process table.
#[derive(Debug, Clone)]
pub struct LiteProc {
    pub name: String,
    pub user: String,
    pub cpu_pct: f32,
    pub rss: u64,
    pub threads: Option<u32>,
    /// CPU history for the row sparkline, 0..1. May be shorter than
    /// `SPARK_W` — `bucket_history` handles any sample count.
    pub history: Vec<f32>,
    // ── detail-only fields ──
    pub pid: u32,
    pub ppid: u32,
    pub state: char,
    pub virt: u64,
    pub io_read_rate: f64,
    pub io_write_rate: f64,
    pub start_time: Option<std::time::SystemTime>,
}

impl LiteProc {
    fn from_tick(t: &ProcTick, history: &History) -> Self {
        let hist = history
            .proc_cpu_history
            .get(&t.pid)
            .map(|r| r.to_vec())
            .unwrap_or_default();
        Self {
            name: t.name.clone(),
            user: t.user.clone(),
            cpu_pct: t.cpu_pct,
            rss: t.mem_rss,
            threads: t.threads,
            history: hist,
            pid: t.pid,
            ppid: t.ppid,
            state: t.state,
            virt: t.mem_virt,
            io_read_rate: t.io_read_rate,
            io_write_rate: t.io_write_rate,
            start_time: t.start_time,
        }
    }
}

/// Top processes by CPU, descending. Lite has exactly one sort order — the
/// full tool's sort keys are among the things Lite cuts.
pub fn collect_procs(snap: &Snapshot, history: &History) -> Vec<LiteProc> {
    let mut rows: Vec<LiteProc> = snap
        .procs
        .iter()
        .map(|t| LiteProc::from_tick(t, history))
        .collect();
    rows.sort_by(|a, b| {
        b.cpu_pct
            .partial_cmp(&a.cpu_pct)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    rows
}

/// Incremental match against process name **or** user, case-insensitive.
pub fn filter_procs(procs: Vec<LiteProc>, query: &str) -> Vec<LiteProc> {
    if query.is_empty() {
        return procs;
    }
    let q = query.to_lowercase();
    procs
        .into_iter()
        .filter(|p| p.name.to_lowercase().contains(&q) || p.user.to_lowercase().contains(&q))
        .collect()
}

// ── Formatters ──────────────────────────────────────────────────────────────

/// CPU percent for the headline number: integer, width 3.
pub fn fmt_cpu_pct(v: f32) -> String {
    format!("{:>3}", v.clamp(0.0, 999.0).round() as u32)
}

/// Memory used for the headline number: width 4 in every regime —
/// `< 10 GB` → ` X.X`, `< 100 GB` → `XX.X`, `>= 100 GB` → ` XXX`.
pub fn fmt_mem_gb(bytes: u64) -> String {
    let gb = bytes as f64 / 1_073_741_824.0;
    if gb >= 100.0 {
        format!("{:>4}", gb.round() as u64)
    } else {
        format!("{:>4.1}", gb)
    }
}

/// Total memory for the `GB / <total>` unit — a rounded integer.
pub fn fmt_total_gb(bytes: u64) -> String {
    format!("{}", (bytes as f64 / 1_073_741_824.0).round() as u64)
}

/// Unavailable vitals render as `--` rather than vanishing: the field keeps
/// its column, so nothing else on the row moves.
pub const UNAVAILABLE: &str = "--";

pub fn fmt_temp(c: Option<f32>) -> String {
    match c {
        Some(c) => format!("{}°C", c.round() as i32),
        None => UNAVAILABLE.into(),
    }
}

pub fn fmt_fan(rpm: Option<u32>) -> String {
    match rpm {
        Some(r) => format!("{}rpm", r),
        None => UNAVAILABLE.into(),
    }
}

pub fn fmt_power(w: Option<f32>) -> String {
    match w {
        Some(w) => format!("{}W", w.round() as u32),
        None => UNAVAILABLE.into(),
    }
}

/// Byte rates on the vitals line and in the detail block.
///
/// **Never wider than 8 columns.** The vitals row is absolute-positioned and
/// the verdict is right-aligned against it, so a rate that renders one column
/// wider than budgeted doesn't wrap — it collides with `memory pressure
/// critical`. Promotion happens at 1000 rather than 1024 so no value can ever
/// round up into a fourth digit (`1024 MB/s` was exactly the 9-column case
/// that overflowed). Units run to PB/s so the contract holds for every input
/// rather than only for plausible ones — the top tier is where a width bound
/// silently stops being true.
pub fn fmt_rate(bytes_per_sec: f64) -> String {
    const UNITS: [&str; 6] = ["B/s", "KB/s", "MB/s", "GB/s", "TB/s", "PB/s"];
    let mut v = bytes_per_sec.max(0.0);
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 || v >= 10.0 {
        format!("{:.0} {}", v, UNITS[u])
    } else {
        format!("{:.1} {}", v, UNITS[u])
    }
}

/// Byte sizes in the table and detail block.
pub fn fmt_bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{} {}", b, UNITS[0])
    } else if v >= 100.0 {
        format!("{:.0} {}", v, UNITS[u])
    } else {
        format!("{:.1} {}", v, UNITS[u])
    }
}

/// The time-axis label, derived from the sample window rather than hardcoded.
///
/// One chart column is one sample, so the window is exactly as wide as the
/// chart — at the reference size and a 1 Hz tick that is `78s ago`, not the
/// `60s ago` of the original design draft, and it changes with the terminal
/// width and the configured tick.
pub fn fmt_window(samples: usize, tick_ms: u64) -> String {
    let secs = (samples as u64).saturating_mul(tick_ms.max(1)) / 1000;
    if secs >= 120 {
        format!(" {}m ago ", secs / 60)
    } else {
        format!(" {}s ago ", secs.max(1))
    }
}

/// Clamp to `w` display columns, dropping from the end.
pub fn truncate_end(s: &str, w: u16) -> String {
    let w = w as usize;
    if s.width() <= w {
        return s.to_string();
    }
    if w == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in s.chars() {
        let cw = ch.to_string().width();
        if used + cw > w.saturating_sub(1) {
            break;
        }
        out.push(ch);
        used += cw;
    }
    out.push('…');
    out
}

// ── Drawing primitives ──────────────────────────────────────────────────────

/// Write `s` at `(x, y)`, clipped so it cannot spill past `clip_x_end`.
fn put(f: &mut Frame, x: u16, y: u16, s: &str, style: Style, clip_x_end: u16) {
    if x > clip_x_end {
        return;
    }
    let max = (clip_x_end - x + 1) as usize;
    f.buffer_mut().set_stringn(x, y, s, max, style);
}

/// Draw right-aligned so the string *ends* on `x_end`.
fn put_right(f: &mut Frame, x_end: u16, y: u16, s: &str, style: Style) {
    let w = s.width() as u16;
    let x = x_end.saturating_sub(w.saturating_sub(1));
    put(f, x, y, s, style, x_end);
}

/// Left-pad a history with zeros to exactly `width` samples.
///
/// Required before handing data to the graph module: `render_bars` right-aligns
/// a short series but `render_dots` indexes cells directly and left-aligns it,
/// so an unpadded series renders in the wrong half of the chart under one style
/// and not the other.
fn pad_to_width(data: &[f32], width: usize) -> Vec<f32> {
    if width == 0 {
        return Vec::new();
    }
    if data.len() >= width {
        return data[data.len() - width..].to_vec();
    }
    let mut out = vec![0.0f32; width - data.len()];
    out.extend_from_slice(data);
    out
}

/// Samples covered by sparkline column `i`, as a half-open range.
pub fn spark_bucket(i: u16, samples: usize) -> std::ops::Range<usize> {
    let lo = (i as usize * samples) / SPARK_W as usize;
    let hi = ((i as usize + 1) * samples) / SPARK_W as usize;
    lo..hi.max(lo)
}

/// Collapse a history of any length to exactly [`SPARK_W`] values, taking the
/// **max** of each bucket so a spike anywhere inside it survives.
fn bucket_history(data: &[f32]) -> Vec<f32> {
    (0..SPARK_W)
        .map(|i| {
            let b = spark_bucket(i, data.len());
            data[b.start..b.end.min(data.len())]
                .iter()
                .copied()
                .fold(0.0f32, f32::max)
        })
        .collect()
}

const BLOCK_GLYPHS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Draw a multi-row chart.
///
/// Under `Dots` this delegates to the shared graph module, whose braille area
/// plot already resolves vertically. Under `Bars` it does **not**: syswatch's
/// shared bar renderer deliberately tiles one glyph down every row of a tile
/// (see `graph::render_bars`), which reads fine on a 3-row KPI cell where the
/// glyph *is* the value, but turns Lite's tall CPU chart into three identical
/// stripes.
///
/// So Bars stacks here instead — eighths accumulated bottom-up, exactly the
/// algorithm in the design handoff and exactly what NetWatch Lite's charts do.
/// Giving CPU three rows is only a claim about importance if those three rows
/// carry three rows' worth of resolution.
fn draw_chart(f: &mut Frame, app: &App, area: Rect, samples: &[f32], color: Color) {
    if area.width == 0 || area.height == 0 || samples.is_empty() {
        return;
    }
    if app.graph_style == graph::GraphStyle::Dots {
        graph::render(f, area, samples, app.graph_style, color, app.graph_opts());
        return;
    }

    let h = area.height as usize;
    let style = Style::default().fg(color).bg(p::bg());
    for (i, v) in samples.iter().take(area.width as usize).enumerate() {
        let x = area.x + i as u16;
        // Total eighth-rows of fill this column earns across the whole chart.
        let eighths = (v.clamp(0.0, 1.0) * (h * 8) as f32).round() as usize;
        for cy in 0..h {
            let from_bottom = h - 1 - cy;
            let in_cell = eighths.saturating_sub(from_bottom * 8).min(8);
            if in_cell == 0 {
                continue;
            }
            if let Some(cell) = f.buffer_mut().cell_mut((x, area.y + cy as u16)) {
                cell.set_char(BLOCK_GLYPHS[in_cell - 1]);
                cell.set_style(style);
            }
        }
    }
}

/// Row sparkline, rendered through the shared graph module so it follows the
/// app-wide bars/dots setting like every other chart in syswatch. A sparkline
/// is one row tall, so stacking and tiling are the same thing here.
fn draw_sparkline(f: &mut Frame, app: &App, x: u16, y: u16, data: &[f32], color: Color) {
    if data.is_empty() {
        return;
    }
    let bucketed = bucket_history(data);
    graph::render(
        f,
        Rect::new(x, y, SPARK_W, 1),
        &bucketed,
        app.graph_style,
        color,
        app.graph_opts(),
    );
}

/// Paint the theme's background across the whole view.
///
/// Lite draws straight to cells rather than rendering ratatui widgets, so
/// without this only the cells it actually touches carry a background — and
/// the chart renderers set one per glyph. On any terminal whose own background
/// differs from the theme's, that showed up as a dark fringe hugging the top
/// edge of every bar (the unfilled half of a partial block glyph) and as chart
/// bands in a visibly different shade from the rest of the screen.
///
/// A no-op under the `terminal` theme, where `p::bg()` is `Color::Reset` —
/// which is exactly what an untouched cell already holds, so the theme that
/// promises to pin no colors still pins none.
fn fill_bg(f: &mut Frame, area: Rect) {
    let bg = p::bg();
    for y in area.y..area.y.saturating_add(area.height) {
        for x in area.x..area.x.saturating_add(area.width) {
            if let Some(cell) = f.buffer_mut().cell_mut((x, y)) {
                cell.set_bg(bg);
            }
        }
    }
}

/// Tint a full-width row for the selection.
fn tint_row(f: &mut Frame, l: &Layout, y: u16, bg: Color) {
    for x in l.content_x..=l.content_x_end() {
        if let Some(cell) = f.buffer_mut().cell_mut((x, y)) {
            cell.set_bg(bg);
        }
    }
}

// ── Render ──────────────────────────────────────────────────────────────────

pub fn render(f: &mut Frame, app: &App, snap: &Snapshot) {
    let area = f.area();
    // Before anything else: every later writer either sets its own background
    // or deliberately leaves one alone (the selection tint), so this has to be
    // the floor rather than a later pass.
    fill_bg(f, area);
    if area.width < GRID_W || area.height < GRID_H {
        render_too_small(f, area);
        return;
    }

    let l = Layout::new(area);
    let paused = app.paused;

    let procs = filter_procs(collect_procs(snap, &app.history), &app.lite.filter_text);
    let matched = procs.len();
    let total = snap.procs.len();

    render_header(f, &l, snap, app, paused);
    render_charts(f, &l, snap, app, paused);
    render_axis(f, &l, app);
    render_vitals(f, &l, snap, app);
    render_table(f, &l, app, &procs, paused);

    if app.lite.filter_input {
        render_prompt(f, &l, app, matched, total);
    }
    render_footer(f, &l);
}

fn render_too_small(f: &mut Frame, area: Rect) {
    let msg = format!(
        "syswatch lite needs {}×{} — this terminal is {}×{}",
        GRID_W, GRID_H, area.width, area.height
    );
    let style = Style::default().fg(p::status_warn());
    let y = area.y + area.height / 2;
    let x = area.x + area.width.saturating_sub(msg.width() as u16) / 2;
    put(f, x, y, &msg, style, area.x + area.width.saturating_sub(1));
}

/// Row 0 — name, host, uptime, and exactly one verdict.
fn render_header(f: &mut Frame, l: &Layout, snap: &Snapshot, app: &App, paused: bool) {
    let y = l.row_procs - ROW_PROCS + ROW_HEADER;
    let end = l.content_x_end();

    let mut x = l.content_x;
    let name = "syswatch";
    put(
        f,
        x,
        y,
        name,
        Style::default()
            .fg(p::text_primary())
            .add_modifier(Modifier::BOLD),
        end,
    );
    x += name.width() as u16;

    let host = format!("  {}", snap.host.hostname);
    put(f, x, y, &host, Style::default().fg(p::status_info()), end);
    x += host.width() as u16;

    let up = format!(" · up {}", fmt_uptime(snap.host.uptime_secs));
    put(f, x, y, &up, Style::default().fg(p::text_muted()), end);

    // The right side carries one thing. Paused wins, then an alert reason,
    // then the load average.
    if paused {
        put_right(
            f,
            end,
            y,
            "◆ PAUSED",
            Style::default()
                .fg(p::status_warn())
                .add_modifier(Modifier::BOLD),
        );
        return;
    }

    let alert = app.lite.alerts.active();
    let (dot_color, text, text_color) = if alert.is_alert() {
        // In alert the load average is replaced by the reason: only one
        // right-aligned string fits, and the reason is the more actionable.
        (
            p::status_error(),
            alert_reason(snap, alert),
            p::status_error(),
        )
    } else {
        (
            p::status_good(),
            format!("load {:.2}", snap.cpu.load_1),
            p::text_muted(),
        )
    };
    let total_w = 2 + text.width() as u16;
    let sx = end + 1 - total_w;
    put(f, sx, y, "●", Style::default().fg(dot_color), end);
    put(f, sx + 2, y, &text, Style::default().fg(text_color), end);
}

/// The short-form reason shown next to the header dot.
fn alert_reason(snap: &Snapshot, alert: Alert) -> String {
    match alert {
        Alert::Thermal => match cpu_temp_c(snap) {
            Some(c) => format!("{}°C throttling", c.round() as i32),
            None => "throttling".into(),
        },
        Alert::SwapThrash => "swap thrashing".into(),
        Alert::MemoryPressure => "memory pressure".into(),
        Alert::None => String::new(),
    }
}

fn fmt_uptime(secs: u64) -> String {
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3600;
    let m = (secs % 3600) / 60;
    if d > 0 {
        format!("{}d {:02}:{:02}", d, h, m)
    } else {
        format!("{:02}:{:02}", h, m)
    }
}

/// Rows 2–8 — two charts sharing one time axis.
fn render_charts(f: &mut Frame, l: &Layout, snap: &Snapshot, app: &App, paused: bool) {
    let top = l.row_procs - ROW_PROCS;
    let end = l.content_x_end();
    let alert = app.lite.alerts.active();

    let cpu_color = if paused {
        p::separator()
    } else if alert == Alert::Thermal {
        p::status_error()
    } else {
        p::rx_rate()
    };
    // The memory series uses status_info, not tx_rate: tx_rate is the natural
    // "second series" slot and is what NetWatch Lite uses for upload, but
    // syswatch's themes define it as magenta rather than the designed cyan.
    let mem_color = if paused {
        p::separator()
    } else {
        p::status_info()
    };

    let dim = Style::default().fg(p::text_muted());
    let bright = Style::default()
        .fg(p::text_primary())
        .add_modifier(Modifier::BOLD);

    // ── CPU ──
    let y = top + ROW_CPU_LABEL;
    put(
        f,
        l.content_x,
        y,
        "◵",
        Style::default().fg(cpu_color).add_modifier(Modifier::BOLD),
        end,
    );
    put_right(
        f,
        l.content_x + CPU_VAL_X_END - CONTENT_X,
        y,
        &fmt_cpu_pct(snap.cpu.usage_pct),
        bright,
    );
    put(
        f,
        l.content_x + CPU_UNIT_X - CONTENT_X,
        y,
        "% cpu",
        dim,
        end,
    );

    let cpu_hist: Vec<f32> = app
        .history
        .cpu
        .iter()
        .map(|v| (*v / 100.0).clamp(0.0, 1.0))
        .collect();
    let window = l.content_w as usize;
    let (peak, avg) = peak_avg(&app.history.cpu, window);
    let cores = if snap.host.cpu_cores > 0 {
        snap.host.cpu_cores as usize
    } else {
        snap.cpu.per_core.len()
    };
    put_right(
        f,
        end,
        y,
        &format!("peak {}  avg {}  {} cores", peak, avg, cores),
        dim,
    );
    draw_chart(
        f,
        app,
        Rect::new(l.content_x, top + ROW_CPU_CHART, l.content_w, CPU_CHART_H),
        &pad_to_width(&cpu_hist, window),
        cpu_color,
    );

    // ── Memory ──
    let y = top + ROW_MEM_LABEL;
    put(
        f,
        l.content_x,
        y,
        "▤",
        Style::default().fg(mem_color).add_modifier(Modifier::BOLD),
        end,
    );
    put_right(
        f,
        l.content_x + MEM_VAL_X_END - CONTENT_X,
        y,
        &fmt_mem_gb(snap.mem.used_bytes),
        bright,
    );
    put(
        f,
        l.content_x + MEM_UNIT_X - CONTENT_X,
        y,
        &format!("GB / {}", fmt_total_gb(snap.mem.total_bytes)),
        dim,
        end,
    );
    put_right(
        f,
        end,
        y,
        &format!(
            "swap {}  pressure {}",
            fmt_bytes(snap.mem.swap_used_bytes),
            mem_pressure(snap).label()
        ),
        dim,
    );

    let mem_hist: Vec<f32> = app.history.mem.iter().map(|v| v.clamp(0.0, 1.0)).collect();
    draw_chart(
        f,
        app,
        Rect::new(l.content_x, top + ROW_MEM_CHART, l.content_w, MEM_CHART_H),
        &pad_to_width(&mem_hist, window),
        mem_color,
    );
}

/// Peak and average over the last `window` samples, as rounded percentages.
fn peak_avg(ring: &crate::collect::Ring<f32>, window: usize) -> (u32, u32) {
    let all = ring.to_vec();
    let slice = if all.len() > window {
        &all[all.len() - window..]
    } else {
        &all[..]
    };
    if slice.is_empty() {
        return (0, 0);
    }
    let peak = slice.iter().copied().fold(0.0f32, f32::max);
    let avg = slice.iter().copied().sum::<f32>() / slice.len() as f32;
    (peak.round() as u32, avg.round() as u32)
}

/// Row 9 — the shared time axis.
/// How far back the leftmost chart column actually reaches.
///
/// Measured from the recorded sample timestamps rather than multiplying the
/// configured tick by the sample count, because the tick is a target the
/// collector does not always meet: on a busy machine, or at an aggressive
/// `--tick`, a sample can cost more than the interval asked for and the loop
/// simply runs late. Multiplying then under-reports the window — a 250ms tick
/// that really costs ~420ms labels 22 seconds of history as "13s ago".
///
/// Falls back to the tick estimate when there aren't two timestamps to
/// subtract, or when they're non-monotonic (a wall-clock adjustment mid-run).
fn window_label(history: &crate::app::History, tick_ms: u64, samples: usize) -> String {
    let measured = (|| {
        let newest = history.session.nth_back(0)?;
        let oldest = history.session.nth_back(samples.checked_sub(1)?)?;
        let secs = newest.t.duration_since(oldest.t).ok()?.as_secs();
        (secs > 0).then_some(secs)
    })();
    match measured {
        Some(secs) if secs >= 120 => format!(" {}m ago ", secs / 60),
        Some(secs) => format!(" {}s ago ", secs),
        None => fmt_window(samples, tick_ms),
    }
}

fn render_axis(f: &mut Frame, l: &Layout, app: &App) {
    let y = l.row_procs - ROW_PROCS + ROW_AXIS;
    let end = l.content_x_end();
    let rule: String = "─".repeat(l.content_w as usize);
    put(
        f,
        l.content_x,
        y,
        &rule,
        Style::default().fg(p::separator()),
        end,
    );

    // The window is however many samples we actually have, capped at the
    // chart width — so a freshly-started syswatch says `12s ago`, not `78s`.
    let samples = app.history.cpu.len().min(l.content_w as usize);
    let dim = Style::default().fg(p::text_muted());
    put(
        f,
        l.content_x,
        y,
        &window_label(&app.history, app.user_config.tick_ms, samples),
        dim,
        end,
    );
    put_right(f, end, y, " now ", dim);
}

/// Row 10 — four vitals at fixed columns, plus the verdict.
fn render_vitals(f: &mut Frame, l: &Layout, snap: &Snapshot, app: &App) {
    let y = l.row_procs - ROW_PROCS + ROW_VITALS;
    let end = l.content_x_end();
    let dim = Style::default().fg(p::text_muted());

    let temp = cpu_temp_c(snap);
    let fan = fan_rpm(snap);
    let values = [
        (fmt_temp(temp), temp.is_some_and(|c| c >= TEMP_WARN_C)),
        (fmt_fan(fan), fan.is_some_and(|r| r >= FAN_WARN_RPM)),
        // Package TDP isn't collected on any platform we support, so power
        // has no threshold to be red against. It stays informational.
        (fmt_power(power_w(snap)), false),
        // Disk throughput is informational by design: a machine reading fast
        // is a machine working, not a machine in trouble.
        (fmt_rate(disk_rate(snap)), false),
    ];

    for (vital, (value, bad)) in VITALS.iter().zip(values.iter()) {
        let lx = l.content_x + vital.label_x - CONTENT_X;
        let vx = l.content_x + vital.val_x - CONTENT_X;
        put(f, lx, y, vital.label, dim, end);
        let style = Style::default().fg(if *bad {
            p::status_error()
        } else {
            p::text_primary()
        });
        // Pad to the fixed field width so a shorter value can't let the
        // previous frame's characters show through.
        put(
            f,
            vx,
            y,
            &format!("{:<width$}", value, width = vital.w as usize),
            style,
            end,
        );
    }

    let alert = app.lite.alerts.active();
    put_right(
        f,
        end,
        y,
        alert.verdict(),
        Style::default().fg(if alert.is_alert() {
            p::status_error()
        } else {
            p::separator()
        }),
    );
}

/// Rows 12–21 — the process table, and the detail block when open.
fn render_table(f: &mut Frame, l: &Layout, app: &App, procs: &[LiteProc], paused: bool) {
    let top = l.row_procs - ROW_PROCS;
    let end = l.content_x_end();
    let dim = Style::default().fg(p::text_muted());

    // Header row, at the same positions as the data.
    let head_y = top + ROW_TABLE_HEAD;
    put(
        f,
        l.x_process,
        head_y,
        &format!("{:<w$}", FIELDS[F_PROCESS].header, w = l.w_process as usize),
        dim,
        end,
    );
    put(
        f,
        l.x_user,
        head_y,
        &format!("{:<w$}", FIELDS[F_USER].header, w = l.w_user as usize),
        dim,
        end,
    );
    let w_rate = FIELDS[F_CPU].w as usize;
    let w_thr = FIELDS[F_THR].w as usize;
    put(
        f,
        l.x_cpu,
        head_y,
        &format!("{:>w$}", FIELDS[F_CPU].header, w = w_rate),
        dim,
        end,
    );
    put(
        f,
        l.x_mem,
        head_y,
        &format!("{:>w$}", FIELDS[F_MEM].header, w = w_rate),
        dim,
        end,
    );
    put(
        f,
        l.x_thr,
        head_y,
        &format!("{:>w$}", FIELDS[F_THR].header, w = w_thr),
        dim,
        end,
    );
    put(
        f,
        l.x_spark,
        head_y,
        &spark_header(app.user_config.tick_ms),
        dim,
        end,
    );

    let rule: String = "─".repeat(l.content_w as usize);
    put(
        f,
        l.content_x,
        top + ROW_RULE,
        &rule,
        Style::default().fg(p::separator()),
        end,
    );

    let detail_open = app.lite.detail_open && !procs.is_empty();
    let visible = l.visible_procs(detail_open) as usize;
    let offset = app.lite.offset;
    // Filtering shows matches only, with no selection tint — the question
    // being asked is "which of these exist", not "which one is selected".
    let selected = if app.lite.filter_input {
        None
    } else {
        Some(app.lite.selected)
    };

    let cpu_color = if paused { p::separator() } else { p::rx_rate() };
    let mem_color = if paused {
        p::separator()
    } else {
        p::status_info()
    };

    for (i, proc) in procs.iter().skip(offset).take(visible).enumerate() {
        let idx = offset + i;
        let y = l.row_procs + i as u16;
        let is_sel = selected == Some(idx);
        if is_sel {
            tint_row(f, l, y, p::selection_bg());
        }

        put(
            f,
            l.x_process,
            y,
            &format!(
                "{:<w$}",
                truncate_end(&proc.name, l.w_process),
                w = l.w_process as usize
            ),
            Style::default().fg(p::text_primary()),
            end,
        );
        put(
            f,
            l.x_user,
            y,
            &format!(
                "{:<w$}",
                truncate_end(&proc.user, l.w_user),
                w = l.w_user as usize
            ),
            dim,
            end,
        );

        // Yellow above 10% — the one transient use of attention color in the
        // table. Not red: a busy process is not a broken machine.
        let hot = proc.cpu_pct > 10.0;
        let cpu_style = Style::default().fg(if paused {
            p::separator()
        } else if hot {
            p::status_warn()
        } else {
            cpu_color
        });
        put(
            f,
            l.x_cpu,
            y,
            &format!("{:>w$}", format!("{:.1}%", proc.cpu_pct), w = w_rate),
            cpu_style,
            end,
        );
        put(
            f,
            l.x_mem,
            y,
            &format!("{:>w$}", fmt_bytes(proc.rss), w = w_rate),
            Style::default().fg(mem_color),
            end,
        );
        let thr = proc
            .threads
            .map(|t| t.to_string())
            .unwrap_or_else(|| UNAVAILABLE.into());
        put(f, l.x_thr, y, &format!("{:>w$}", thr, w = w_thr), dim, end);

        draw_sparkline(
            f,
            app,
            l.x_spark,
            y,
            &proc.history,
            if is_sel { cpu_color } else { p::separator() },
        );
    }

    if detail_open {
        if let Some(proc) = procs.get(app.lite.selected) {
            let rendered = procs.len().saturating_sub(offset).min(visible) as u16;
            let y = l.row_procs + rendered;
            render_detail(f, l, proc, y);
        }
    }
}

/// The three-row expansion under the list. Progressive disclosure: this is
/// what replaces the full TUI's entire Procs tab.
fn render_detail(f: &mut Frame, l: &Layout, proc: &LiteProc, y: u16) {
    let end = l.content_x_end();
    let dim = Style::default().fg(p::text_muted());
    put(
        f,
        l.content_x + 2,
        y,
        "└─",
        Style::default().fg(p::separator()),
        end,
    );

    let x = l.content_x + 5;
    put(
        f,
        x,
        y,
        &format!(
            "pid {}   ppid {}   state {}   user {}",
            proc.pid, proc.ppid, proc.state, proc.user
        ),
        dim,
        end,
    );
    put(
        f,
        x,
        y + 1,
        &format!(
            "rss {}   virt {}   read {}   write {}",
            fmt_bytes(proc.rss),
            fmt_bytes(proc.virt),
            fmt_rate(proc.io_read_rate),
            fmt_rate(proc.io_write_rate),
        ),
        dim,
        end,
    );
    let threads = proc
        .threads
        .map(|t| format!("{} threads", t))
        .unwrap_or_else(|| "threads --".into());
    put(
        f,
        x,
        y + 2,
        &format!(
            "started {}   cpu {:.1}%   {}",
            fmt_started(proc.start_time),
            proc.cpu_pct,
            threads
        ),
        dim,
        end,
    );
}

fn fmt_started(t: Option<std::time::SystemTime>) -> String {
    match t {
        Some(t) => {
            let dt: chrono::DateTime<chrono::Local> = t.into();
            dt.format("%H:%M").to_string()
        }
        None => UNAVAILABLE.into(),
    }
}

/// Row 22 — the filter prompt, only while typing.
fn render_prompt(f: &mut Frame, l: &Layout, app: &App, matched: usize, total: usize) {
    let y = l.row_prompt;
    let end = l.content_x_end();
    put(
        f,
        l.content_x,
        y,
        "/",
        Style::default()
            .fg(p::status_warn())
            .add_modifier(Modifier::BOLD),
        end,
    );
    let text = &app.lite.filter_text;
    let x = l.content_x + 2;
    put(f, x, y, text, Style::default().fg(p::text_primary()), end);
    put(
        f,
        x + text.width() as u16,
        y,
        "█",
        Style::default().fg(p::text_primary()),
        end,
    );
    put(
        f,
        x + text.width() as u16 + 3,
        y,
        &format!("{} of {} match", matched, total),
        Style::default().fg(p::text_muted()),
        end,
    );
}

/// Row 23 — the entire key surface.
fn render_footer(f: &mut Frame, l: &Layout) {
    let y = l.row_footer;
    let end = l.content_x_end();
    let mut x = l.content_x;
    for (i, (key, label)) in FOOTER_KEYS.iter().enumerate() {
        if i > 0 {
            x += FOOTER_GAP;
        }
        put(
            f,
            x,
            y,
            key,
            Style::default()
                .fg(p::key_hint())
                .add_modifier(Modifier::BOLD),
            end,
        );
        x += key.width() as u16;
        let label = format!(" {}", label);
        put(f, x, y, &label, Style::default().fg(p::text_muted()), end);
        x += label.width() as u16;
    }
    // The keys are the point of the row; the version is a courtesy. On a
    // terminal too narrow for both, the version is what goes.
    let version = footer_version();
    if footer_keys_width() + 1 + version.width() as u16 <= l.content_w {
        put_right(f, end, y, &version, Style::default().fg(p::separator()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference() -> Rect {
        Rect::new(0, 0, GRID_W, GRID_H)
    }

    #[test]
    fn fields_tile_without_overlap_and_end_on_the_content_edge() {
        let mut prev_end = CONTENT_X - 1;
        for f in FIELDS {
            assert!(
                f.x > prev_end,
                "field {} starts at {} but the previous field ended at {}",
                f.header,
                f.x,
                prev_end
            );
            prev_end = f.x_end();
        }
        assert_eq!(
            prev_end,
            CONTENT_X + CONTENT_W - 1,
            "the last field must end exactly on the content edge"
        );
    }

    #[test]
    fn layout_at_reference_size_matches_spec() {
        let l = Layout::new(reference());
        assert_eq!(l.content_x, CONTENT_X);
        assert_eq!(l.content_w, CONTENT_W);
        // The column positions the handoff calls authoritative, and which
        // NetWatch Lite shares character-for-character.
        assert_eq!(l.x_process, 1);
        assert_eq!(l.x_user, 17);
        assert_eq!(l.w_user, 22);
        assert_eq!(l.x_cpu, 40);
        assert_eq!(l.x_mem, 51);
        assert_eq!(l.x_thr, 62);
        assert_eq!(l.x_spark, 70);
        assert_eq!(l.row_procs, ROW_PROCS);
        assert_eq!(l.proc_rows, PROC_ROWS);
        assert_eq!(l.row_prompt, ROW_PROMPT);
        assert_eq!(l.row_footer, ROW_FOOTER);
        assert_eq!(l.visible_procs(true), PROC_ROWS - DETAIL_ROWS);
    }

    #[test]
    fn layout_columns_match_the_fields_table() {
        let l = Layout::new(reference());
        let xs = [l.x_process, l.x_user, l.x_cpu, l.x_mem, l.x_thr, l.x_spark];
        for (f, x) in FIELDS.iter().zip(xs.iter()) {
            assert_eq!(f.x, *x, "field {} moved", f.header);
        }
    }

    #[test]
    fn wide_terminal_gives_the_surplus_to_user() {
        let l = Layout::new(Rect::new(0, 0, 160, 40));
        assert_eq!(l.content_w, 158);
        assert_eq!(l.x_process, 1);
        assert_eq!(l.w_process, W_PROCESS);
        // Everything right of USER keeps its width; USER absorbs the rest.
        assert_eq!(l.x_spark, 158 + 1 - SPARK_W + 1 - 1);
        assert_eq!(l.content_x_end() - l.x_spark + 1, SPARK_W);
        assert!(l.w_user > 22, "USER should absorb the surplus width");
        // And the list absorbs the surplus height.
        assert!(l.proc_rows > PROC_ROWS);
    }

    #[test]
    fn footer_fits_beside_the_version_at_the_reference_size() {
        let keys = footer_keys_width();
        let version = footer_version().width() as u16;
        assert!(
            keys + 1 + version <= CONTENT_W,
            "footer keys ({}) + version ({}) overflow {} columns",
            keys,
            version,
            CONTENT_W
        );
    }

    #[test]
    fn vitals_fields_do_not_collide_and_leave_room_for_the_verdict() {
        let mut prev_end = 0;
        for v in VITALS {
            assert!(v.label_x > prev_end, "vital {} collides", v.label);
            let label_end = v.label_x + v.label.width() as u16 - 1;
            assert!(
                v.val_x > label_end,
                "vital {} value overlaps its label",
                v.label
            );
            prev_end = v.val_x + v.w - 1;
        }
        // Every verdict must fit right-aligned after the last vital — not
        // just the one that happens to be longest today.
        for alert in [
            Alert::None,
            Alert::Thermal,
            Alert::SwapThrash,
            Alert::MemoryPressure,
        ] {
            let verdict = alert.verdict().width() as u16;
            let verdict_start = CONTENT_X + CONTENT_W - verdict;
            assert!(
                verdict_start > prev_end + 1,
                "{:?} verdict {:?} starts at {} but vitals end at {} — they need a blank column between them",
                alert,
                alert.verdict(),
                verdict_start,
                prev_end
            );
        }
    }

    #[test]
    fn rates_never_exceed_their_column_budget() {
        // Every magnitude, including the 1023/1024 rounding boundaries that
        // are exactly where a fourth digit sneaks in.
        let mut v = 0.0f64;
        while v < 1.0e15 {
            let s = fmt_rate(v);
            assert!(
                s.width() <= 8,
                "fmt_rate({}) = {:?} is {} columns",
                v,
                s,
                s.width()
            );
            v = if v == 0.0 { 1.0 } else { v * 1.37 };
        }
        for boundary in [1023.0, 1024.0, 1_048_575.0, 1_048_576.0, 1_073_741_824.0] {
            let s = fmt_rate(boundary);
            assert!(s.width() <= 8, "fmt_rate({}) = {:?}", boundary, s);
        }
        assert!(fmt_rate(1024.0 * 1_048_576.0).ends_with("GB/s"));
    }

    #[test]
    fn paging_and_starvation_get_different_verdicts() {
        // A machine holding a big swap file while paging nothing is starved,
        // not thrashing. Saying "thrashing" is how red stops being believed.
        let mut snap = Snapshot::default();
        snap.mem.total_bytes = 18 * 1_073_741_824;
        snap.mem.available_bytes = 1_073_741_824; // <10% headroom
        snap.mem.swap_used_bytes = 13 * 1_073_741_824;
        assert_eq!(mem_pressure(&snap), Pressure::Critical);

        let mut starved = AlertTracker::default();
        for _ in 0..ALERT_FIRE_SAMPLES {
            starved.update(&snap, Some(0.0)); // critical, but not paging
        }
        assert_eq!(starved.active(), Alert::MemoryPressure);
        assert_eq!(starved.active().verdict(), "memory pressure critical");

        // Actively paging is the acute form and outranks it.
        let mut thrashing = AlertTracker::default();
        for _ in 0..ALERT_FIRE_SAMPLES {
            thrashing.update(&snap, Some(SWAP_ALERT_BPS * 2.0));
        }
        assert_eq!(thrashing.active(), Alert::SwapThrash);
    }

    #[test]
    fn swap_rate_is_measured_against_the_tick() {
        let mut h = History::new(10);
        let mut snap = Snapshot::default();
        snap.mem.swap_used_bytes = 100 * 1_048_576;
        h.push(&snap);
        snap.mem.swap_used_bytes = 120 * 1_048_576; // +20 MB
        h.push(&snap);

        // 20 MB in one second.
        let r = swap_rate(&h, 1000).unwrap();
        assert!((r - 20.0 * 1_048_576.0).abs() < 1.0, "got {}", r);
        // The same delta over two seconds is half the rate — the threshold
        // has to mean the same thing at any configured tick.
        let r2 = swap_rate(&h, 2000).unwrap();
        assert!((r2 - 10.0 * 1_048_576.0).abs() < 1.0, "got {}", r2);
        // Swap being freed is churn too, not a negative rate.
        snap.mem.swap_used_bytes = 0;
        h.push(&snap);
        assert!(swap_rate(&h, 1000).unwrap() > 0.0);
    }

    #[test]
    fn headline_numbers_never_reach_their_units() {
        // The whole point of the fixed widths: no value can push the unit.
        for pct in [0.0f32, 7.4, 24.0, 99.6, 100.0] {
            let s = fmt_cpu_pct(pct);
            assert_eq!(s.width(), 3, "cpu {} rendered as {:?}", pct, s);
        }
        for gb in [0.5f64, 4.2, 19.8, 99.9, 128.0, 512.0] {
            let s = fmt_mem_gb((gb * 1_073_741_824.0) as u64);
            assert_eq!(s.width(), 4, "mem {} rendered as {:?}", gb, s);
        }
    }

    #[test]
    fn vital_values_fit_their_fields() {
        let cases: [(usize, String); 4] = [
            (0, fmt_temp(Some(100.0))),
            (1, fmt_fan(Some(4800))),
            (2, fmt_power(Some(100.0))),
            (3, fmt_rate(1024.0 * 1024.0 * 1024.0)),
        ];
        for (i, value) in cases {
            assert!(
                value.width() <= VITALS[i].w as usize,
                "{} needs {} columns but {} is {} wide",
                value,
                value.width(),
                VITALS[i].label,
                VITALS[i].w
            );
        }
        // And the unavailable marker fits everywhere.
        for v in VITALS {
            assert!(UNAVAILABLE.width() <= v.w as usize);
        }
    }

    #[test]
    fn alerts_need_three_samples_to_fire_and_five_to_clear() {
        let mut h = Hysteresis::default();
        assert!(!h.update(true));
        assert!(!h.update(true));
        assert!(h.update(true), "third consecutive sample should fire");
        // One good sample must not clear it.
        assert!(h.update(false));
        assert!(h.update(false));
        assert!(h.update(false));
        assert!(h.update(false));
        assert!(!h.update(false), "fifth quiet sample should clear");
    }

    #[test]
    fn an_alternating_condition_never_fires() {
        // The reason hysteresis exists: a machine sitting on a threshold
        // must not flap red, or nobody believes red again.
        let mut h = Hysteresis::default();
        for i in 0..50 {
            assert!(!h.update(i % 2 == 0), "flapped on sample {}", i);
        }
    }

    #[test]
    fn missing_sensors_never_raise_an_alert() {
        let mut t = AlertTracker::default();
        let snap = Snapshot::default(); // no thermal zones, no throttle flag
        for _ in 0..10 {
            t.update(&snap, None);
        }
        assert_eq!(t.active(), Alert::None);
    }

    #[test]
    fn a_throttle_flag_fires_thermal() {
        let mut t = AlertTracker::default();
        let mut snap = Snapshot::default();
        snap.power.thermal_throttle_pct = Some(70);
        for _ in 0..ALERT_FIRE_SAMPLES {
            t.update(&snap, None);
        }
        assert_eq!(t.active(), Alert::Thermal);
        assert_eq!(t.active().verdict(), "cpu thermal throttling");
    }

    #[test]
    fn thermal_outranks_swap() {
        let mut t = AlertTracker::default();
        let mut snap = Snapshot::default();
        snap.power.thermal_throttle_pct = Some(50);
        for _ in 0..ALERT_FIRE_SAMPLES {
            t.update(&snap, Some(SWAP_ALERT_BPS * 2.0));
        }
        assert_eq!(t.active(), Alert::Thermal);
    }

    #[test]
    fn window_label_is_derived_from_samples_and_tick() {
        // The reference grid at 1 Hz — 78 columns, one sample each.
        assert_eq!(fmt_window(78, 1000), " 78s ago ");
        // A freshly-started syswatch says what it actually has.
        assert_eq!(fmt_window(12, 1000), " 12s ago ");
        // A slower tick covers more wall-clock in the same columns.
        assert_eq!(fmt_window(78, 2000), " 2m ago ");
    }

    /// Lite must sit on the theme's background, not the terminal's — otherwise
    /// the per-glyph backgrounds the chart renderers set show up as a fringe
    /// against whatever the terminal happens to use.
    ///
    /// Under the `terminal` theme the opposite must hold: `p::bg()` is
    /// `Color::Reset` there, and a theme whose entire contract is pinning no
    /// colors must not start painting one now.
    #[test]
    fn the_background_is_filled_by_the_theme_and_reset_under_terminal() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        // The active theme is process-global; take the same turn-taking lock
        // the theme module's own mutating tests use.
        let _guard = crate::ui::theme::exclusive_theme();

        let fill = |theme: &str| {
            crate::ui::theme::set_by_name(theme);
            let mut term = Terminal::new(TestBackend::new(GRID_W, GRID_H)).unwrap();
            term.draw(|f| {
                let a = f.area();
                fill_bg(f, a);
            })
            .unwrap();
            let buf = term.backend().buffer().clone();
            // A corner no other renderer writes to.
            buf[(GRID_W - 1, GRID_H - 1)].bg
        };

        assert_eq!(fill("dark"), crate::ui::theme::by_name("dark").bg);
        assert_ne!(
            fill("dark"),
            Color::Reset,
            "the dark theme really does pin a background, so this test can fail"
        );
        assert_eq!(fill("terminal"), Color::Reset);

        crate::ui::theme::set_by_name("dark");
    }

    /// The axis window must measure the history it is labelling, not restate
    /// the tick that was asked for. A collector that can't keep up with an
    /// aggressive `--tick` runs late, and multiplying count by interval then
    /// under-reports the window by however far behind it fell.
    #[test]
    fn window_label_measures_real_elapsed_time_not_the_requested_tick() {
        use std::time::Duration;

        // Asked for 4 Hz, but each sample really cost 500ms — so the loop
        // ran late and the history spans twice what the tick implies.
        let mut h = crate::app::History::new(120);
        let base = std::time::SystemTime::UNIX_EPOCH;
        for i in 0..40u64 {
            h.push(&Snapshot {
                t: base + Duration::from_millis(i * 500),
                ..Default::default()
            });
        }

        // 40 samples, 39 gaps of 500ms = 19s of real history. The tick
        // estimate would have claimed 40 * 250ms = 10s.
        assert_eq!(window_label(&h, 250, 40), " 19s ago ");
        assert_eq!(
            fmt_window(40, 250),
            " 10s ago ",
            "precondition: the estimate really is the wrong answer here"
        );

        // A partial window measures only the columns actually shown.
        assert_eq!(window_label(&h, 250, 11), " 5s ago ");
    }

    /// With nothing to subtract, fall back rather than render a bare "0s".
    #[test]
    fn window_label_falls_back_before_two_samples_exist() {
        let mut h = crate::app::History::new(120);
        assert_eq!(window_label(&h, 1000, 0), fmt_window(0, 1000));

        h.push(&Snapshot::default());
        assert_eq!(window_label(&h, 1000, 1), fmt_window(1, 1000));
    }

    /// The sparkline column header must describe the span it actually shows.
    /// It shipped as a hardcoded "60s" against an 8-sample ring — wrong at
    /// every tick, and most visibly at the 1 Hz default.
    #[test]
    fn spark_header_tracks_the_tick_and_the_ring_depth() {
        // Tied to the ring, not to a number typed twice: if the depth
        // changes, this test follows it rather than going quietly stale.
        let depth = crate::app::PROC_CPU_SPARK_LEN as u64;
        assert_eq!(spark_header(1000), format!("{depth}s"));
        assert_eq!(spark_header(500), format!("{}s", depth / 2));
        assert_eq!(spark_header(60_000), format!("{}m", depth));

        // Never wider than the column it labels, at any tick.
        for tick in [50, 250, 1000, 2000, 10_000, 600_000] {
            assert!(
                spark_header(tick).width() <= SPARK_W as usize,
                "header for tick {tick} overflows the sparkline column"
            );
        }
        // And never empty, however fast the tick.
        assert_eq!(spark_header(1), "1s");
    }

    #[test]
    fn spark_buckets_tile_the_whole_history() {
        for samples in [1usize, 8, 9, 60, 78, 120] {
            let mut prev_end = 0;
            for i in 0..SPARK_W {
                let b = spark_bucket(i, samples);
                assert_eq!(b.start, prev_end, "gap before bucket {}", i);
                prev_end = b.end;
            }
            assert_eq!(prev_end, samples, "buckets dropped tail samples");
        }
    }

    #[test]
    fn bucketing_keeps_spikes() {
        // A single spike anywhere must survive the downsample — that is the
        // whole reason buckets take the max rather than sampling.
        let mut data = vec![0.0f32; 60];
        data[37] = 1.0;
        let bucketed = bucket_history(&data);
        assert_eq!(bucketed.len(), SPARK_W as usize);
        assert!(bucketed.contains(&1.0), "the spike vanished");
    }

    #[test]
    fn padding_right_aligns_now_on_the_chart_edge() {
        let data = [0.1f32, 0.2, 0.3];
        let padded = pad_to_width(&data, 6);
        assert_eq!(padded, vec![0.0, 0.0, 0.0, 0.1, 0.2, 0.3]);
        // Longer than the chart keeps the newest samples.
        let long: Vec<f32> = (0..10).map(|i| i as f32).collect();
        assert_eq!(pad_to_width(&long, 3), vec![7.0, 8.0, 9.0]);
    }

    #[test]
    fn filter_matches_name_or_user() {
        let mk = |name: &str, user: &str| LiteProc {
            name: name.into(),
            user: user.into(),
            cpu_pct: 0.0,
            rss: 0,
            threads: None,
            history: Vec::new(),
            pid: 1,
            ppid: 0,
            state: 'R',
            virt: 0,
            io_read_rate: 0.0,
            io_write_rate: 0.0,
            start_time: None,
        };
        let procs = vec![
            mk("firefox", "jules"),
            mk("Code Helper", "jules"),
            mk("kernel_task", "root"),
        ];
        assert_eq!(filter_procs(procs.clone(), "helper").len(), 1);
        assert_eq!(filter_procs(procs.clone(), "root").len(), 1);
        assert_eq!(filter_procs(procs.clone(), "").len(), 3);
        assert_eq!(filter_procs(procs, "ZZZ").len(), 0);
    }

    #[test]
    fn pressure_falls_back_when_psi_is_absent() {
        let mut snap = Snapshot::default();
        snap.mem.total_bytes = 32 * 1_073_741_824;
        snap.mem.available_bytes = 16 * 1_073_741_824;
        assert_eq!(mem_pressure(&snap), Pressure::None);

        snap.mem.swap_used_bytes = 1_073_741_824;
        assert_eq!(mem_pressure(&snap), Pressure::Warning);

        snap.mem.available_bytes = 1_073_741_824; // ~3% headroom
        assert_eq!(mem_pressure(&snap), Pressure::Critical);
    }

    #[test]
    fn psi_wins_over_the_fallback_when_present() {
        let mut snap = Snapshot::default();
        snap.mem.total_bytes = 32 * 1_073_741_824;
        snap.mem.available_bytes = 16 * 1_073_741_824;
        snap.mem.swap_used_bytes = 1_073_741_824; // would say Warning
        snap.pressure = Some(crate::collect::PressureTick {
            mem_full: 25.0,
            ..Default::default()
        });
        assert_eq!(mem_pressure(&snap), Pressure::Critical);
    }

    /// The shape of an ordinary, healthy Mac: memory held full, a multi-GB
    /// swap file, and a kernel calmly reporting "warning". The swap/headroom
    /// inference calls this Critical and lights the header red; the kernel
    /// must be believed over it, or red stops meaning anything.
    #[test]
    fn the_kernel_level_outranks_the_swap_inference() {
        use crate::collect::MemPressureLevel;

        let mut snap = Snapshot::default();
        snap.mem.total_bytes = 18 * 1_073_741_824;
        snap.mem.available_bytes = 1_073_741_824; // ~6% headroom
        snap.mem.swap_used_bytes = 6 * 1_073_741_824;
        assert_eq!(
            mem_pressure(&snap),
            Pressure::Critical,
            "precondition: the inference alone would fire"
        );

        snap.mem.pressure_level = Some(MemPressureLevel::Warning);
        assert_eq!(mem_pressure(&snap), Pressure::Warning);

        snap.mem.pressure_level = Some(MemPressureLevel::Normal);
        assert_eq!(mem_pressure(&snap), Pressure::None);

        // And it still fires when the kernel genuinely says so.
        snap.mem.pressure_level = Some(MemPressureLevel::Critical);
        assert_eq!(mem_pressure(&snap), Pressure::Critical);
    }

    /// PSI outranks the macOS level too — not that one machine has both, but
    /// the precedence must be total rather than incidental.
    #[test]
    fn psi_outranks_the_kernel_level() {
        let mut snap = Snapshot::default();
        snap.mem.total_bytes = 32 * 1_073_741_824;
        snap.mem.pressure_level = Some(crate::collect::MemPressureLevel::Critical);
        snap.pressure = Some(crate::collect::PressureTick::default());
        assert_eq!(mem_pressure(&snap), Pressure::None);
    }

    #[test]
    fn cpu_temp_prefers_a_named_zone_over_the_hottest() {
        let mut snap = Snapshot::default();
        snap.power.thermal_zones = vec![
            crate::collect::ThermalZone {
                name: "GPU".into(),
                temp_c: 88.0,
            },
            crate::collect::ThermalZone {
                name: "CPU package".into(),
                temp_c: 61.0,
            },
        ];
        assert_eq!(cpu_temp_c(&snap), Some(61.0));

        // With nothing named, the hottest zone stands in.
        snap.power.thermal_zones = vec![crate::collect::ThermalZone {
            name: "TZ0".into(),
            temp_c: 44.0,
        }];
        assert_eq!(cpu_temp_c(&snap), Some(44.0));
    }

    #[test]
    fn truncation_is_display_width_aware() {
        assert_eq!(truncate_end("firefox", 15), "firefox");
        assert_eq!(truncate_end("com.apple.WebKit.WebContent", 10).width(), 10);
        // CJK names are two columns per character, so a naive char count
        // would overflow the field by half its width.
        let cjk = "网络监视器进程";
        assert!(truncate_end(cjk, 8).width() <= 8);
    }

    #[test]
    fn processes_sort_by_cpu_descending() {
        let snap = Snapshot {
            procs: vec![
                ProcTick {
                    name: "quiet".into(),
                    cpu_pct: 0.4,
                    ..Default::default()
                },
                ProcTick {
                    name: "busy".into(),
                    cpu_pct: 42.0,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let history = History::new(120);
        let rows = collect_procs(&snap, &history);
        assert_eq!(rows[0].name, "busy");
    }
}
