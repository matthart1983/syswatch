//! SysWatch Dense — the high-density six-box screen.
//!
//! The third view, after the twelve-tab Full TUI and the deliberately minimal
//! [`crate::ui::lite`]. Where Lite answers *why is my machine hot, slow or
//! loud?* on 80×24 with six keys, Dense answers *what is this machine doing,
//! all of it, at once* — every subsystem on one screen at 130×44, built on the
//! craft decisions that make btop world-class rather than merely functional.
//!
//! It is the sibling of netwatch's Dense view: same primitives, same panel
//! idiom, same `V` cycle. Only the subject changes. **Preserve that symmetry**
//! the way `lite.rs` does.
//!
//! # Zero chrome rows
//!
//! No header bar, no tab bar, no status bar. Identity, uptime, aggregate, sort
//! state, page range and every keybind live *inside the box borders* — that is
//! what [`paint::panel`] buys. Every row of the terminal carries content.
//!
//! # The mirror means "two directions of one flow"
//!
//! Only `net` earns one: download grows up from a shared axis, upload grows
//! down, so traffic symmetry becomes a shape — a restore is a cliff above the
//! line, a backup a cliff below it. `cpu` and `mem` have no opposing
//! direction, so each gets one honest full-height graph rather than a
//! manufactured partner. `disk` takes the small slot as two independent
//! read/write sparklines: its rate is bursty but its story is one line.
//! Temperature is a bounded scalar, not a flow, so it sits on the vitals row
//! with a green→red meter. **Don't extend the mirror to things that aren't
//! flows** — it is the load-bearing idea of this layout.
//!
//! # Every number is derived from the series it sits beside
//!
//! Peaks, averages and axis ceilings are all measured off the *visible window*
//! of the same ring the graph draws, never off the whole ring and never passed
//! in separately. A label physically cannot disagree with its pixels. See
//! [`Series`].
//!
//! # Degrading
//!
//! Metrics this platform doesn't expose render `--` in dim and **never shift
//! the layout**. PSI is Linux-only, fan RPM and package power are macOS/laptop
//! -only, and per-core frequency isn't collected at all — the boxes keep their
//! rows regardless, because a layout that reflows per platform can't be
//! learned.

pub mod paint;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame;
use std::cell::{Cell, RefCell};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, History};
use crate::collect::{ProcTick, Snapshot};
use crate::ui::theme::{self, Theme};
use crate::ui::widgets::human_bytes;
use paint::{Bind, PanelOpts, Ramp, Ramps};

// ── the reference grid ──────────────────────────────────────────────────────
//
// 130×44 is the size the design was drawn at. It is *not* a cap: `Layout`
// generalises to whatever terminal is attached, and falls back to the compact
// arrangement when there isn't room for six boxes.

pub const GRID_W: u16 = 130;
pub const GRID_H: u16 = 44;

/// Band heights at the reference size. cpu and the mid band are fixed; procs
/// takes everything left over, so a taller terminal buys process rows rather
/// than stretched graphs.
const H_CPU: u16 = 12;
const H_MID: u16 = 12;
const H_SMALL: u16 = 8;
const MIN_PROCS: u16 = 5;

/// Below either of these the six-box arrangement stops being legible — the
/// side-by-side halves get too narrow to carry a graph plus its axis gutter,
/// or the bands crowd procs out entirely. Compact is not a degraded Dense; it
/// is the same information at the density the terminal can actually hold.
const MIN_FULL_H: u16 = H_CPU + H_MID + H_SMALL + MIN_PROCS;
const MIN_FULL_W: u16 = 100;

/// The floor for Compact itself. Below this every box is border and no
/// interior, so Dense says what it needs instead of drawing a broken grid.
const MIN_DENSE_W: u16 = 56;
const MIN_DENSE_H: u16 = 14;

/// Axis gutter, relative to a box's interior origin: labels right-align to
/// `AX_TICK - 1`, the tick sits at `AX_TICK`, one blank column, then the graph.
/// One constant for every box so the value ticks and the time tick land in the
/// same column and the left edge reads as a single straight rule. Placing an
/// axis by hand is what produces a one-column stair between a scale row and
/// the axis row directly below it.
const AX_TICK: u16 = 5;
const AX_GRAPH: u16 = AX_TICK + 2;

/// Dense's own view state. Separate from Full's `proc_sel` and from
/// [`crate::app::LiteState`] for the same reason those two are separate: the
/// views have different row sets, and sharing a cursor would make cycling
/// views move the other view's selection.
#[derive(Debug, Clone, Default)]
pub struct DenseState {
    /// Last known row index — the fallback when the selected process exits.
    pub selected: usize,
    /// The process the cursor is actually on. The table re-sorts every tick,
    /// so an index alone slides the cursor onto a *different* process
    /// underneath the user; the detail block then flickers between processes
    /// nobody asked for.
    pub selected_pid: Option<u32>,
    /// The box expanded to fill the frame, by the bracketed hotkey it carries
    /// in its own border. `None` is the six-box grid.
    pub zoom: Option<u8>,
    /// Eased display ceilings for the auto-scaled rate graphs, in bytes/sec.
    ///
    /// `Cell` because only rendering knows a graph's width, and therefore its
    /// visible window, and `render` takes `&App`. See [`ease_ceiling`].
    net_ceiling: Cell<u64>,
    io_ceiling: Cell<u64>,
    /// The order the rows were last drawn in, by pid. See [`sorted_procs`].
    order: RefCell<Vec<u32>>,
}

/// Move a graph's ceiling toward `target`, rising at once and falling slowly.
///
/// A ceiling taken straight from the visible peak flaps: one burst rescales
/// the whole graph, and when that burst scrolls out of the window the trace
/// collapses back to the baseline in a single frame. Measured on a live run,
/// the network ceiling walked 3.9K → 5.9K → 488K → 5.9K inside twelve seconds,
/// which is most of what "janky" means here.
///
/// Rising is immediate because a clipped spike is a lie. Falling is damped, so
/// a quiet patch after a burst eases back into scale instead of snapping.
fn ease_ceiling(slot: &Cell<u64>, target: u64) -> u64 {
    let cur = slot.get();
    let next = if cur == 0 || target >= cur {
        target
    } else {
        // ~8% of the gap per frame, with a floor so it actually arrives
        // instead of asymptoting a few percent above target forever.
        let step = ((cur - target) / 12).max(1);
        (cur - step).max(target)
    };
    slot.set(next);
    next
}

/// The process table in the order it is drawn.
///
/// Sorted on the **smoothed** CPU figure, not the instantaneous one. Raw
/// `cpu_pct` puts six processes within a percent of each other and they trade
/// places every single tick — the table reads as a slot machine. The EWMA
/// (alpha 0.3, ~5 ticks) is already maintained for the runaway-process
/// heuristic and is exactly the right key here. The *displayed* number stays
/// instantaneous and truthful; only the ordering is damped.
///
/// PID breaks ties so the order is total, and equal-load processes cannot
/// swap on a whim.
pub fn sorted_procs<'a>(snap: &'a Snapshot, h: &History, st: &DenseState) -> Vec<&'a ProcTick> {
    let prev: std::collections::HashMap<u32, usize> = st
        .order
        .borrow()
        .iter()
        .enumerate()
        .map(|(i, p)| (*p, i))
        .collect();
    let key = |p: &ProcTick| h.proc_cpu_ewma.get(&p.pid).copied().unwrap_or(p.cpu_pct);
    let rank = |pid: u32| prev.get(&pid).copied().unwrap_or(usize::MAX);

    let mut v: Vec<&ProcTick> = snap.procs.iter().collect();
    v.sort_by(|a, b| {
        band(key(b))
            .cmp(&band(key(a)))
            .then_with(|| rank(a.pid).cmp(&rank(b.pid)))
            .then_with(|| a.pid.cmp(&b.pid))
    });
    *st.order.borrow_mut() = v.iter().map(|p| p.pid).collect();
    v
}

/// The load band a process sorts into.
///
/// Ordering on a smoothed *value* still flips every time two processes cross,
/// and on a busy machine half a dozen of them sit permanently within noise of
/// each other — so the table trades places every tick and reads as a slot
/// machine. Banding makes the comparison coarse: inside a band the previous
/// order simply stands, and a row only overtakes another when its load
/// genuinely separates.
///
/// Two percentage points. Wide enough that jitter never crosses it, narrow
/// enough that a real change in load still reorders the table promptly. The
/// long idle tail all lands in band 0 and stops moving altogether.
///
/// Note this is a *total, transitive* order — bands, then previous rank, then
/// pid. A comparator that returned "equal if within tolerance" would not be,
/// and `sort_by` gives unspecified results for those.
fn band(pct: f32) -> i32 {
    (pct.max(0.0) / 2.0) as i32
}

/// Which row the cursor is on, resolved by PID against the current order.
/// Falls back to the last index when that process has exited.
pub fn selected_index_by_pid(pid: Option<u32>, fallback: usize, pids: &[u32]) -> usize {
    if pids.is_empty() {
        return 0;
    }
    pid.and_then(|want| pids.iter().position(|p| *p == want))
        .unwrap_or_else(|| fallback.min(pids.len() - 1))
}

/// [`selected_index_by_pid`] against the rows themselves.
pub fn selected_index(state: &DenseState, procs: &[&ProcTick]) -> usize {
    let pids: Vec<u32> = procs.iter().map(|p| p.pid).collect();
    selected_index_by_pid(state.selected_pid, state.selected, &pids)
}

// ── layout ──────────────────────────────────────────────────────────────────

/// Which arrangement the attached terminal can hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// Six boxes: cpu / mem+net / cores+disk / procs.
    Full,
    /// Three boxes: cpu / io / procs. disk and net collapse into one `io` box
    /// where each keeps its read/write and down/up pairing on a single row.
    Compact,
}

#[derive(Debug, Clone, Copy)]
pub struct Layout {
    pub shape: Shape,
    /// Set when one box has been expanded to fill the frame; every other rect
    /// is then zero-sized and its box draws nothing.
    pub zoom: Option<u8>,
    pub cpu: Rect,
    /// `mem` in Full, the combined `io` box in Compact.
    pub mem: Rect,
    /// `net` in Full; zero-sized in Compact.
    pub net: Rect,
    /// `cores` in Full; zero-sized in Compact.
    pub cores: Rect,
    /// `disk` in Full; zero-sized in Compact.
    pub disk: Rect,
    pub procs: Rect,
}

impl Layout {
    pub fn new(area: Rect) -> Self {
        let w = area.width;
        let (x, y) = (area.x, area.y);
        if area.height < MIN_FULL_H || w < MIN_FULL_W {
            // Compact: cpu gets what's left after a fixed io strip and a procs
            // box with a floor, so the graph shrinks before the table does.
            let io_h = 4.min(area.height.saturating_sub(2));
            let procs_h = ((area.height.saturating_sub(io_h)) / 2)
                .clamp(MIN_PROCS.min(area.height), area.height.saturating_sub(io_h));
            let cpu_h = area.height.saturating_sub(io_h + procs_h);
            return Self {
                shape: Shape::Compact,
                zoom: None,
                cpu: Rect::new(x, y, w, cpu_h),
                mem: Rect::new(x, y + cpu_h, w, io_h),
                net: Rect::new(x, y, 0, 0),
                cores: Rect::new(x, y, 0, 0),
                disk: Rect::new(x, y, 0, 0),
                procs: Rect::new(x, y + cpu_h + io_h, w, procs_h),
            };
        }
        let left = w / 2;
        let right = w - left;
        let procs_h = area.height - (H_CPU + H_MID + H_SMALL);
        Self {
            shape: Shape::Full,
            zoom: None,
            cpu: Rect::new(x, y, w, H_CPU),
            mem: Rect::new(x, y + H_CPU, left, H_MID),
            net: Rect::new(x + left, y + H_CPU, right, H_MID),
            cores: Rect::new(x, y + H_CPU + H_MID, left, H_SMALL),
            disk: Rect::new(x + left, y + H_CPU + H_MID, right, H_SMALL),
            procs: Rect::new(x, y + H_CPU + H_MID + H_SMALL, w, procs_h),
        }
    }

    /// The hotkeys this arrangement actually offers. Compact has three boxes,
    /// so `4`–`6` must not silently blank the screen.
    pub fn boxes(&self) -> &'static [u8] {
        match self.shape {
            Shape::Full => &[1, 2, 3, 4, 5, 6],
            Shape::Compact => &[1, 2, 3],
        }
    }

    /// One box expanded to fill the frame.
    ///
    /// The bracketed `┤1├` in every border is an affordance; leaving it inert
    /// is worse than not drawing it. Zoom is the behaviour it implies and the
    /// one that earns its place on a screen this dense — sometimes you want
    /// the process table, or just the network mirror, at full size.
    pub fn with_zoom(area: Rect, zoom: Option<u8>) -> Self {
        let base = Self::new(area);
        let Some(id) = zoom.filter(|z| base.boxes().contains(z)) else {
            return base;
        };
        let e = Rect::new(area.x, area.y, 0, 0);
        let mut l = Self {
            shape: base.shape,
            zoom: Some(id),
            cpu: e,
            mem: e,
            net: e,
            cores: e,
            disk: e,
            procs: e,
        };
        match (base.shape, id) {
            (_, 1) => l.cpu = area,
            (_, 2) => l.mem = area,
            (Shape::Compact, 3) => l.procs = area,
            (Shape::Full, 3) => l.net = area,
            (Shape::Full, 4) => l.cores = area,
            (Shape::Full, 5) => l.disk = area,
            (Shape::Full, 6) => l.procs = area,
            _ => return base,
        }
        l
    }

    /// The layout at the size the design was drawn at. Handy in tests and as
    /// the thing to compare against when changing band heights.
    #[cfg(test)]
    pub fn reference() -> Self {
        Self::new(Rect::new(0, 0, GRID_W, GRID_H))
    }

    /// Process rows the table can show, after its header and the two-row
    /// detail block hoisted into the top of the same box.
    pub fn visible_procs(&self) -> usize {
        let interior = self.procs.height.saturating_sub(2);
        interior.saturating_sub(3) as usize
    }
}

// ── series ──────────────────────────────────────────────────────────────────

/// Samples per character column in every time-series graph.
///
/// A braille cell addresses two horizontal sub-positions and [`paint::area_graph`]
/// supports that. But the collector produces **one sample per redraw**, so the
/// second sub-position buys no extra detail — it only doubles the time span,
/// and it costs twice as long to fill the graph plus a half-column shift per
/// tick that re-renders the whole trace (measured: ~50% more glyph churn per
/// frame than a whole-column scroll). One sample per column fills a 120-column
/// graph in two minutes instead of four and scrolls exactly one column a tick.
///
/// Raise to 2 if the sampler is ever driven faster than the redraw; the
/// primitive and its tests already handle it.
const SAMPLES_PER_COL: usize = 1;

/// The visible window of a ring, plus every statistic printed beside it.
///
/// Built once per graph and used for *both* the pixels and the labels, which
/// is what makes it impossible for the two to disagree. `vals` is exactly
/// `2 × width` long — the graph's own resolution — so `peak` is the peak you
/// can actually see, not one hiding in scrolled-off history.
pub struct Series {
    pub vals: Vec<u64>,
    pub peak: u64,
    pub avg: u64,
    pub cur: u64,
    /// Auto-scaled ceiling, or the caller's fixed one for a semantic axis.
    pub max: u64,
}

impl Series {
    /// Rates, auto-scaled. `width` is the graph's column count.
    fn rates(ring: impl Iterator<Item = f64>, width: u16) -> Self {
        Self::build(ring.map(|v| v.max(0.0) as u64).collect(), width, None)
    }

    /// A series on a fixed, semantically meaningful ceiling — a percentage, a
    /// total-memory axis. These must never auto-scale: the empty space above
    /// the trace *is* the reading.
    fn fixed(vals: Vec<u64>, width: u16, max: u64) -> Self {
        Self::build(vals, width, Some(max))
    }

    fn build(all: Vec<u64>, width: u16, fixed: Option<u64>) -> Self {
        let want = (width as usize) * SAMPLES_PER_COL;
        let start = all.len().saturating_sub(want);
        let vals: Vec<u64> = all[start..].to_vec();
        let peak = vals.iter().copied().max().unwrap_or(0);
        let avg = if vals.is_empty() {
            0
        } else {
            (vals.iter().map(|&v| v as u128).sum::<u128>() / vals.len() as u128) as u64
        };
        let cur = vals.last().copied().unwrap_or(0);
        let max = fixed.unwrap_or_else(|| nice_ceil(peak));
        Self {
            vals,
            peak,
            avg,
            cur,
            max,
        }
    }

    /// The samples as [`paint::area_graph`] wants them — two entries per
    /// character column. At one sample per column each sample simply fills its
    /// whole cell.
    fn plot(&self) -> Vec<u64> {
        match SAMPLES_PER_COL {
            2 => self.vals.clone(),
            _ => self.vals.iter().flat_map(|&v| [v, v]).collect(),
        }
    }
}

/// Smallest "nice" value at or just above `v`.
///
/// The mantissa series is deliberately finer than a 1/2/5 decade: no two
/// adjacent steps are more than 1.25× apart, so an auto-scaled graph never
/// wastes more than ~20% of its height, and in practice wastes under 10%. A
/// coarse series is not a cosmetic matter — a 64→96 jump leaves the top row of
/// a graph permanently empty, which throws away the braille resolution the
/// whole design is built on.
pub fn nice_ceil(v: u64) -> u64 {
    if v == 0 {
        return 1;
    }
    const MANTISSAS: [u64; 13] = [
        100, 125, 150, 175, 200, 250, 300, 350, 400, 500, 600, 700, 800,
    ];
    let mut scale = 1u64;
    loop {
        for m in MANTISSAS {
            // m is in hundredths, so the step is m * scale / 100.
            let c = m.saturating_mul(scale) / 100;
            if c >= v {
                return c.max(1);
            }
        }
        match scale.checked_mul(10) {
            Some(s) => scale = s,
            None => return u64::MAX,
        }
    }
}

// ── entry point ─────────────────────────────────────────────────────────────

pub fn render(f: &mut Frame, app: &App, snap: &Snapshot) {
    let area = f.area();
    let t = theme::active();
    let ramps = Ramps::from_theme(&t);
    let l = Layout::with_zoom(area, app.dense.zoom);

    // Paint the ground once. Dense fills every cell it owns, so a partially
    // drawn frame never shows the previous view through the gaps.
    let buf = f.buffer_mut();
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(' ');
                cell.set_style(Style::default().bg(t.bg));
            }
        }
    }

    // Below the compact arrangement's own floor there is nothing honest to
    // draw: boxes would be all border and no interior. Say so, and name the
    // size the design actually wants, rather than painting a broken grid.
    if area.width < MIN_DENSE_W || area.height < MIN_DENSE_H {
        let msg = format!(
            "syswatch dense wants {GRID_W}×{GRID_H} (min {MIN_DENSE_W}×{MIN_DENSE_H}) — this terminal is {}×{}",
            area.width, area.height
        );
        let y = area.y + area.height / 2;
        let x = area.x + area.width.saturating_sub(msg.width() as u16) / 2;
        paint::put(buf, x, y, &msg, Style::default().fg(t.status_warn));
        return;
    }

    match l.shape {
        Shape::Full => {
            cpu_box(buf, l.cpu, &t, &ramps, app, snap);
            mem_box(buf, l.mem, &t, &ramps, app, snap);
            net_box(buf, l.net, &t, &ramps, app, snap);
            cores_box(buf, l.cores, &t, &ramps, app, snap);
            disk_box(buf, l.disk, &t, &ramps, app, snap);
        }
        Shape::Compact => {
            cpu_box(buf, l.cpu, &t, &ramps, app, snap);
            io_box(buf, l.mem, &t, &ramps, app, snap);
        }
    }
    procs_box(buf, l.procs, &t, &ramps, app, snap, &l);
}

// ── box 1: cpu ──────────────────────────────────────────────────────────────

fn cpu_box(buf: &mut Buffer, area: Rect, t: &Theme, r: &Ramps, app: &App, snap: &Snapshot) {
    if area.height < 5 {
        return;
    }
    let up = fmt_uptime(snap.host.uptime_secs);
    let right = format!("syswatch  {}  up {}", snap.host.hostname, up);
    let cores = snap.cpu.per_core.len();
    let sub = if cores > 0 {
        format!("{} · {} cores", trim_cpu_model(&snap.host.cpu_model), cores)
    } else {
        trim_cpu_model(&snap.host.cpu_model).to_string()
    };
    let binds: &[Bind] = &[
        ("V", " view"),
        (",", " menu"),
        ("1-6", " zoom"),
        ("?", " keys"),
    ];
    let inner = paint::panel(
        buf,
        area,
        t,
        &PanelOpts {
            key: Some("1"),
            title: Some("cpu"),
            sub: Some(&sub),
            right: Some(&right),
            foot_left: binds,
            // Says why the top of the graph is empty at idle: the axis is
            // fixed, and that blank space is the machine's headroom.
            foot_right: Some("axis 0-100% fixed"),
            ..Default::default()
        },
    );
    if inner.height < 4 || inner.width <= AX_GRAPH + 4 {
        return;
    }

    let gx = inner.x + AX_GRAPH;
    let gw = inner.width - AX_GRAPH - 1; // 1-column right gutter
    let graph_h = inner.height - 3; // headline, axis, vitals
                                    // A percentage has a real ceiling. Fixed axis, always.
                                    // Carried at 10× so `peak` and `avg` keep the same decimal the headline
                                    // shows; rounding to whole percent here would print "peak 25" beside a
                                    // headline of 24.8 and look like a disagreement.
    let s = Series::fixed(
        app.history
            .cpu
            .iter()
            .map(|&v| (v.clamp(0.0, 100.0) * 10.0).round() as u64)
            .collect(),
        gw,
        1000,
    );

    // headline
    let y = inner.y;
    // FIXED COLUMNS, right-aligned numbers. Chaining these off a running
    // cursor slid every label to the right by a column the moment a value
    // crossed a digit boundary (9.9 → 10.0), so the headline twitched once a
    // second. A monitor's numbers change constantly; its furniture must not.
    paint::put(
        buf,
        inner.x + 1,
        y,
        "▲",
        Style::default().fg(r.primary.mid()),
    );
    paint::put_right(
        buf,
        inner.x + 8,
        y,
        &format!("{:.1}", snap.cpu.usage_pct),
        Style::default()
            .fg(t.text_primary)
            .add_modifier(Modifier::BOLD),
    );
    paint::put(
        buf,
        inner.x + 10,
        y,
        "% cpu",
        Style::default().fg(t.text_muted),
    );
    paint::put(
        buf,
        inner.x + 19,
        y,
        "peak",
        Style::default().fg(t.text_muted),
    );
    paint::put_right(
        buf,
        inner.x + 29,
        y,
        &format!("{:.1}", s.peak as f32 / 10.0),
        Style::default().fg(t.text_muted),
    );
    paint::put(
        buf,
        inner.x + 33,
        y,
        "avg",
        Style::default().fg(t.text_muted),
    );
    paint::put_right(
        buf,
        inner.x + 42,
        y,
        &format!("{:.1}", s.avg as f32 / 10.0),
        Style::default().fg(t.text_muted),
    );
    let la = format!(
        "load {:.2}  {:.2}  {:.2}",
        snap.cpu.load_1, snap.cpu.load_5, snap.cpu.load_15
    );
    paint::put_right(
        buf,
        inner.right() - 2,
        y,
        &la,
        Style::default().fg(t.text_muted),
    );

    // graph + fixed scale
    let gy = inner.y + 1;
    tick(buf, inner.x, gy, "100%", t);
    if graph_h >= 3 {
        tick(buf, inner.x, gy + graph_h / 2, "50%", t);
    }
    tick(buf, inner.x, gy + graph_h - 1, "0", t);
    paint::area_graph(
        buf,
        Rect::new(gx, gy, gw, graph_h),
        &s.plot(),
        s.max,
        &r.primary,
        false,
    );

    // The time axis is its own row and carries nothing else, so every tick
    // sits at its true position.
    let ay = gy + graph_h;
    time_axis(buf, inner, ay, gx, gw, t, app);

    // vitals
    vitals_row(buf, inner, ay + 1, t, r, snap);
}

/// `oldest ┤────┤ mid ├────┤ now ├`, with the midpoint tick on the true middle
/// column of the graph rather than wherever the label happens to centre.
fn time_axis(buf: &mut Buffer, inner: Rect, y: u16, gx: u16, gw: u16, t: &Theme, app: &App) {
    let border = Style::default().fg(t.border);
    for x in gx..gx + gw {
        paint::set(buf, x, y, '─', border);
    }
    let span = window_secs(&app.history, gw);
    tick_styled(
        buf,
        inner.x,
        y,
        &fmt_secs(span),
        t,
        Style::default().fg(t.text_muted),
    );
    if gw > 24 {
        let mid = gx + (gw - 1) / 2;
        let lab = format!("┤ {} ├", fmt_secs(span / 2));
        let w = lab.width() as u16;
        paint::put(
            buf,
            mid.saturating_sub(w / 2),
            y,
            &lab,
            Style::default().fg(t.text_muted),
        );
    }
    paint::put_right(
        buf,
        inner.right() - 2,
        y,
        "┤ now ├",
        Style::default().fg(t.text_muted),
    );
}

fn vitals_row(buf: &mut Buffer, inner: Rect, y: u16, t: &Theme, r: &Ramps, snap: &Snapshot) {
    let mut x = paint::put(
        buf,
        inner.x + 1,
        y,
        "vitals",
        Style::default().fg(t.text_muted),
    );
    x += 2;

    // Temperature: bounded scalar, so a meter — never a graph.
    let temp = snap
        .power
        .thermal_zones
        .iter()
        .map(|z| z.temp_c)
        .fold(f32::NAN, f32::max);
    if temp.is_finite() {
        let hot = temp >= 85.0;
        x = paint::put(
            buf,
            x,
            y,
            &format!("{temp:.0}°C"),
            Style::default()
                .fg(if hot { t.status_error } else { t.text_primary })
                .add_modifier(Modifier::BOLD),
        );
        x = paint::put(buf, x + 2, y, "thermal", Style::default().fg(t.text_muted));
        let mw = 14u16.min(inner.right().saturating_sub(x + 3));
        // 30..100°C is the band worth showing: below 30 nothing is happening,
        // above 100 the machine has already thermally throttled.
        paint::meter(
            buf,
            x + 1,
            y,
            mw,
            (temp - 30.0) / 70.0,
            &r.load,
            t.text_muted,
        );
        x += 1 + mw + 2;
    } else {
        x = paint::put(buf, x, y, "temp --", Style::default().fg(t.text_muted));
        x += 2;
    }

    // Labels dim, values bright. The vitals row is a list of small readings,
    // and drawing the whole line in one muted token gives the eye nothing to
    // land on — the numbers are the point, the words are scaffolding.
    if let Some(f) = snap.power.fans.iter().map(|f| f.rpm).max() {
        x = paint::put(buf, x, y, "fan", Style::default().fg(t.text_muted));
        x = paint::put(
            buf,
            x + 1,
            y,
            &format!("{f}rpm"),
            Style::default().fg(t.text_primary),
        );
        x += 2;
    }
    if let Some(w) = snap.power.system_power_w {
        x = paint::put(
            buf,
            x,
            y,
            &format!("{w:.1}W"),
            Style::default().fg(t.text_primary),
        );
        x += 2;
    }
    // PSI is the one signal btop doesn't surface at all. Linux-only; on other
    // platforms the field renders `--` rather than vanishing.
    let psi = snap
        .pressure
        .as_ref()
        .map(|p| format!("{:.1}%", p.cpu_some))
        .unwrap_or_else(|| "--".into());
    x = paint::put(buf, x, y, "psi cpu", Style::default().fg(t.text_muted));
    paint::put(
        buf,
        x + 1,
        y,
        &psi,
        Style::default().fg(match snap.pressure.as_ref() {
            Some(p) if p.cpu_some >= 10.0 => t.status_error,
            Some(_) => t.text_primary,
            None => t.text_muted,
        }),
    );

    // `thermal_throttle_pct` is a CPU SPEED LIMIT, not an amount of throttling:
    // macOS `pmset -g therm` reports 100 for a machine running flat out. Read
    // it the same way Lite does — inverting this reports every idle Mac as
    // thermally throttled.
    let (verdict, vstyle) = match snap.power.thermal_throttle_pct {
        Some(p) if p < 100 => (
            format!("throttle ACTIVE · clocks capped {p}%"),
            t.status_error,
        ),
        Some(_) => ("throttle none".to_string(), t.text_muted),
        None => (String::new(), t.text_muted),
    };
    if !verdict.is_empty() {
        paint::put_right(
            buf,
            inner.right() - 2,
            y,
            &verdict,
            Style::default().fg(vstyle),
        );
    }
}

// ── box 2: mem ──────────────────────────────────────────────────────────────

fn mem_box(buf: &mut Buffer, area: Rect, t: &Theme, r: &Ramps, app: &App, snap: &Snapshot) {
    let total = snap.mem.total_bytes;
    let (label, style) = match snap.mem.pressure_level {
        Some(crate::collect::MemPressureLevel::Critical) => {
            ("● CRITICAL", Style::default().fg(t.status_error))
        }
        Some(crate::collect::MemPressureLevel::Warning) => {
            ("● PRESSURE", Style::default().fg(t.status_warn))
        }
        Some(crate::collect::MemPressureLevel::Normal) => {
            ("● OK", Style::default().fg(t.status_good))
        }
        None => ("", Style::default()),
    };
    let sub = human_bytes(total);
    let inner = paint::panel(
        buf,
        area,
        t,
        &PanelOpts {
            key: Some("2"),
            title: Some("mem"),
            sub: Some(&sub),
            right: (!label.is_empty()).then_some(label),
            right_style: Some(style),
            foot_right: Some(&if snap.mem.swap_total_bytes > 0 {
                format!(
                    "swap {} / {}",
                    human_bytes(snap.mem.swap_used_bytes),
                    human_bytes(snap.mem.swap_total_bytes)
                )
            } else {
                format!(
                    "{} free",
                    human_bytes(total.saturating_sub(snap.mem.used_bytes))
                )
            }),
            ..Default::default()
        },
    );
    if inner.height < 6 || inner.width < 24 {
        return;
    }

    // Composition meters. Three rows, always the same three, so the box has a
    // fixed shape no matter which of them is zero.
    let mw = (inner.width / 2).min(36);
    // Each meter carries its OWN denominator. Measuring swap against installed
    // RAM is the classic version of this mistake — "swap 36%" then means 36% of
    // RAM, a number describing nothing, when what you want is how much of the
    // swap file is spoken for.
    //
    // `free` is derived rather than read from `available_bytes`: macOS reports
    // available as ~0 essentially always, because it holds every spare page as
    // cache. A permanently empty meter reading "0 B" looks like a broken
    // widget, and total-minus-used is both honest and useful.
    let swap_denom = if snap.mem.swap_total_bytes > 0 {
        snap.mem.swap_total_bytes
    } else {
        total
    };
    let rows: [(&str, u64, u64, &Ramp); 3] = [
        ("used", snap.mem.used_bytes, total, &r.load),
        (
            "free",
            total.saturating_sub(snap.mem.used_bytes),
            total,
            &r.secondary,
        ),
        ("swap", snap.mem.swap_used_bytes, swap_denom, &r.load),
    ];
    for (i, (lab, bytes, denom, ramp)) in rows.iter().enumerate() {
        let y = inner.y + i as u16;
        paint::put(buf, inner.x + 1, y, lab, Style::default().fg(t.text_muted));
        let frac = if *denom > 0 {
            *bytes as f32 / *denom as f32
        } else {
            0.0
        };
        paint::meter(buf, inner.x + 8, y, mw, frac, ramp, t.text_muted);
        let vx = inner.x + 8 + mw + 1;
        paint::put(
            buf,
            vx,
            y,
            &human_bytes(*bytes),
            Style::default().fg(t.text_primary),
        );
        paint::put_right(
            buf,
            inner.right() - 2,
            y,
            &format!("{:>3.0}%", frac * 100.0),
            Style::default().fg(t.text_muted),
        );
    }

    // Separator carrying the derived session average, then the history graph.
    let sep_y = inner.y + 3;
    let gy = sep_y + 1;
    let graph_h = inner.height.saturating_sub(5);
    if graph_h == 0 || inner.width <= AX_GRAPH + 4 {
        return;
    }
    let gx = inner.x + AX_GRAPH;
    let gw = inner.width - AX_GRAPH - 1;
    // Total memory is a real ceiling — fixed axis, like a percentage.
    let s = Series::fixed(
        app.history
            .mem
            .iter()
            .map(|&f| (f.clamp(0.0, 1.0) as f64 * total as f64) as u64)
            .collect(),
        gw,
        total.max(1),
    );

    let border = Style::default().fg(t.border);
    for x in inner.x + 1..inner.right() - 1 {
        paint::set(buf, x, sep_y, '─', border);
    }
    paint::put(
        buf,
        inner.x + 1,
        sep_y,
        "┤ resident ├",
        Style::default().fg(t.text_muted),
    );
    paint::put_right(
        buf,
        inner.right() - 2,
        sep_y,
        &format!("┤ {} avg ├", human_bytes(s.avg)),
        Style::default().fg(t.text_muted),
    );

    tick(buf, inner.x, gy, &bytes_axis(total), t);
    tick(buf, inner.x, gy + graph_h - 1, "0", t);
    paint::area_graph(
        buf,
        Rect::new(gx, gy, gw, graph_h),
        &s.plot(),
        s.max,
        &r.secondary,
        false,
    );

    // PSI stall — the thing btop doesn't show.
    let py = inner.y + inner.height - 1;
    let mut x = paint::put(
        buf,
        inner.x + 1,
        py,
        "psi stall",
        Style::default().fg(t.text_muted),
    );
    x += 1;
    match snap.pressure.as_ref() {
        Some(p) => {
            for (k, v) in [("cpu", p.cpu_some), ("mem", p.mem_some), ("io", p.io_some)] {
                x = paint::put(buf, x, py, k, Style::default().fg(t.border));
                x = paint::put(
                    buf,
                    x + 1,
                    py,
                    &format!("{v:.1}%"),
                    Style::default().fg(if v >= 10.0 {
                        t.status_error
                    } else {
                        t.text_primary
                    }),
                );
                x += 2;
            }
        }
        None => {
            paint::put(
                buf,
                x,
                py,
                "cpu --  mem --  io --",
                Style::default().fg(t.text_muted),
            );
        }
    }
}

// ── box 3: net (the mirror) ─────────────────────────────────────────────────

fn net_box(buf: &mut Buffer, area: Rect, t: &Theme, r: &Ramps, app: &App, snap: &Snapshot) {
    let iface = snap
        .net
        .iter()
        .filter(|i| i.is_up)
        .max_by(|a, b| {
            (a.rx_rate + a.tx_rate)
                .partial_cmp(&(b.rx_rate + b.tx_rate))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|i| i.name.clone())
        .unwrap_or_else(|| "no link".into());
    let up_count = snap.net.iter().filter(|i| i.is_up).count();
    let inner = paint::panel(
        buf,
        area,
        t,
        &PanelOpts {
            key: Some("3"),
            title: Some("net"),
            sub: Some(&iface),
            foot_right: Some(&format!("{up_count} up")),
            ..Default::default()
        },
    );
    if inner.height < 7 || inner.width <= AX_GRAPH + 6 {
        return;
    }
    let gx = inner.x + AX_GRAPH;
    let gw = inner.width - AX_GRAPH - 1;

    // Both halves of a mirrored pair share one ceiling. Scaling them
    // independently would make a 2 MB/s upload look the same size as a
    // 200 MB/s download, which is the one comparison the mirror exists to make.
    let down = Series::rates(app.history.net_rx.iter().copied(), gw);
    let up = Series::rates(app.history.net_tx.iter().copied(), gw);
    let max = ease_ceiling(&app.dense.net_ceiling, nice_ceil(down.peak.max(up.peak)));

    // The interior carries five things: the down readout, the down graph, the
    // shared axis, the up graph, and the up readout. Only the two graphs are
    // elastic, so THREE rows come off the top before splitting the remainder —
    // budgeting for two would push the up readout onto the box's own border.
    let body = inner.height.saturating_sub(3);
    let down_h = body.div_ceil(2);
    let up_h = body - down_h;

    mirror_half(
        buf, inner, inner.y, gx, gw, down_h, t, &r.primary, &down, max, "↓", "down", false,
    );
    let ay = inner.y + 1 + down_h;
    let border = Style::default().fg(t.border);
    for x in gx..gx + gw {
        paint::set(buf, x, ay, '─', border);
    }
    tick_styled(
        buf,
        inner.x,
        ay,
        &fmt_secs(window_secs(&app.history, gw)),
        t,
        Style::default().fg(t.text_muted),
    );
    paint::put_right(
        buf,
        inner.right() - 2,
        ay,
        "┤ now ├",
        Style::default().fg(t.text_muted),
    );
    mirror_half(
        buf,
        inner,
        ay + up_h + 1,
        gx,
        gw,
        up_h,
        t,
        &r.secondary,
        &up,
        max,
        "↑",
        "up",
        true,
    );
}

/// One half of the mirrored pair. `flip` puts the graph below the axis growing
/// down, and moves the readout row to the bottom so it sits on the outside of
/// the pair rather than between the two traces.
#[allow(clippy::too_many_arguments)]
fn mirror_half(
    buf: &mut Buffer,
    inner: Rect,
    label_y: u16,
    gx: u16,
    gw: u16,
    h: u16,
    t: &Theme,
    ramp: &Ramp,
    s: &Series,
    max: u64,
    glyph: &str,
    name: &str,
    flip: bool,
) {
    if h == 0 {
        return;
    }
    let gy = if flip { label_y - h } else { label_y + 1 };
    paint::area_graph(buf, Rect::new(gx, gy, gw, h), &s.plot(), max, ramp, flip);
    // Scale labels ordered by growth direction: the ceiling always sits at the
    // far edge from the shared axis.
    let (ceil_y, zero_y) = if flip {
        (gy + h - 1, gy)
    } else {
        (gy, gy + h - 1)
    };
    tick(buf, inner.x, ceil_y, &rate_short(max), t);
    tick(buf, inner.x, zero_y, "0", t);

    paint::put(
        buf,
        inner.x + 1,
        label_y,
        glyph,
        Style::default().fg(ramp.mid()),
    );
    // Right-aligned in a fixed field so `down` / `up` hold still while the
    // rate walks between two and six characters wide.
    paint::put_right(
        buf,
        inner.x + 9,
        label_y,
        &rate_short(s.cur),
        Style::default()
            .fg(t.text_primary)
            .add_modifier(Modifier::BOLD),
    );
    paint::put(
        buf,
        inner.x + 11,
        label_y,
        name,
        Style::default().fg(t.text_muted),
    );
    paint::put_right(
        buf,
        inner.right() - 2,
        label_y,
        &format!("peak {}", rate_short(s.peak)),
        Style::default().fg(t.text_muted),
    );
}

// ── box 4: cores ────────────────────────────────────────────────────────────

fn cores_box(buf: &mut Buffer, area: Rect, t: &Theme, r: &Ramps, app: &App, snap: &Snapshot) {
    let n = snap.cpu.per_core.len();
    let sub = format!("{n}");
    let inner = paint::panel(
        buf,
        area,
        t,
        &PanelOpts {
            key: Some("4"),
            title: Some("cores"),
            sub: Some(&sub),
            right: Some(&format!("aggregate {:.1}%", snap.cpu.usage_pct)),
            // The rows are in topology order, which is what you want when
            // reading one core against its neighbour. Say so rather than
            // claiming a sort the rows don't obey.
            foot_right: Some("order topology"),
            ..Default::default()
        },
    );
    if inner.height < 2 || n == 0 {
        return;
    }
    // Two columns where there's room, so twice the cores fit the same rows.
    let cols: u16 = if inner.width >= 60 { 2 } else { 1 };
    let col_w = inner.width / cols;
    let rows = inner.height - 1;
    let capacity = (rows * cols) as usize;

    for c in 0..cols {
        let x = inner.x + c * col_w;
        paint::put(buf, x + 1, inner.y, "CORE", Style::default().fg(t.border));
        paint::put_right(buf, x + 10, inner.y, "UTIL", Style::default().fg(t.border));
        paint::put(buf, x + 13, inner.y, "60s", Style::default().fg(t.border));
    }

    for (i, &pct) in snap.cpu.per_core.iter().take(capacity).enumerate() {
        let col = (i as u16) % cols;
        let row = (i as u16) / cols;
        let x = inner.x + col * col_w;
        let y = inner.y + 1 + row;
        paint::put(
            buf,
            x + 1,
            y,
            &format!("C{i}"),
            Style::default().fg(t.text_secondary),
        );
        paint::put_right(
            buf,
            x + 10,
            y,
            &format!("{pct:.0}%"),
            Style::default().fg(if pct > 80.0 {
                t.status_warn
            } else {
                t.text_primary
            }),
        );
        let sw = col_w.saturating_sub(15);
        if sw >= 4 {
            match app.history.per_core.get(i) {
                Some(ring) if ring.len() > 1 => {
                    // Perceptual scaling: eight dot levels can't carry 1–95%
                    // linearly, and a flat band would render every core
                    // identically at exactly the moment you need to tell them
                    // apart.
                    let vals: Vec<u64> = ring
                        .iter()
                        .map(|&v| paint::perceptual(v / 100.0, 64))
                        .collect();
                    let s = Series::fixed(vals, sw, 64);
                    paint::spark(buf, x + 13, y, sw, &s.plot(), s.max, &r.primary);
                }
                _ => paint::baseline(buf, x + 13, y, sw, r.dim.mid()),
            }
        }
    }
}

// ── box 5: disk ─────────────────────────────────────────────────────────────

fn disk_box(buf: &mut Buffer, area: Rect, t: &Theme, r: &Ramps, app: &App, snap: &Snapshot) {
    let dev = snap
        .disks
        .iter()
        .max_by_key(|d| d.total_bytes)
        .map(|d| d.device.clone())
        .unwrap_or_else(|| "disk".into());
    let inner = paint::panel(
        buf,
        area,
        t,
        &PanelOpts {
            key: Some("5"),
            title: Some("disk"),
            sub: Some(&dev),
            foot_right: Some(&format!("{} vols", snap.disks.len())),
            ..Default::default()
        },
    );
    if inner.height < 3 || inner.width < 30 {
        return;
    }
    // Read and write keep their primary/secondary pairing as independent
    // sparklines — disk's rate is bursty but its story is one line, so it gets
    // the small slot rather than a mirror it hasn't earned.
    //
    // The sparkline's width is DERIVED from the peak label that follows it
    // plus a gutter, so the two can never share a column however wide either
    // value grows. Sizing it off the box width instead is what lets a graph
    // grow into its own annotation.
    let spark_x = inner.x + 22;
    let peak_w = 12u16; // "pk 1023.9M" and then some
    let spark_w = inner
        .right()
        .saturating_sub(2 + peak_w)
        .saturating_sub(spark_x)
        .max(4);
    let read = Series::rates(app.history.io_read.iter().copied(), spark_w);
    let write = Series::rates(app.history.io_write.iter().copied(), spark_w);
    let max = ease_ceiling(&app.dense.io_ceiling, nice_ceil(read.peak.max(write.peak)));

    for (i, (g, name, s, ramp)) in [
        ("r", "read", &read, &r.primary),
        ("w", "write", &write, &r.secondary),
    ]
    .iter()
    .enumerate()
    {
        let y = inner.y + i as u16;
        paint::put(buf, inner.x + 1, y, g, Style::default().fg(ramp.mid()));
        paint::put(buf, inner.x + 3, y, name, Style::default().fg(t.text_muted));
        paint::put_right(
            buf,
            inner.x + 20,
            y,
            &rate_short(s.cur),
            Style::default()
                .fg(t.text_primary)
                .add_modifier(Modifier::BOLD),
        );
        paint::spark(buf, spark_x, y, spark_w, &s.plot(), max, ramp);
        paint::put_right(
            buf,
            inner.right() - 2,
            y,
            &format!("pk {}", rate_short(s.peak)),
            Style::default().fg(t.text_muted),
        );
    }

    // Below a rule: capacity, which is the other half of "how is the disk".
    if inner.height < 5 {
        return;
    }
    let sep_y = inner.y + 2;
    let border = Style::default().fg(t.border);
    for x in inner.x + 1..inner.right() - 1 {
        paint::set(buf, x, sep_y, '─', border);
    }
    for (i, d) in snap
        .disks
        .iter()
        .filter(|d| d.total_bytes > 0)
        .take((inner.height - 3) as usize)
        .enumerate()
    {
        let y = sep_y + 1 + i as u16;
        let mp = truncate(&d.mount_point, 10);
        paint::put(
            buf,
            inner.x + 1,
            y,
            &mp,
            Style::default().fg(t.text_secondary),
        );
        let mw = (inner.width / 3).min(20);
        paint::meter(
            buf,
            inner.x + 12,
            y,
            mw,
            d.usage_pct / 100.0,
            &r.load,
            t.text_muted,
        );
        paint::put_right(
            buf,
            inner.right() - 2,
            y,
            &format!(
                "{} / {}  {:.0}%",
                human_bytes(d.used_bytes),
                human_bytes(d.total_bytes),
                d.usage_pct
            ),
            Style::default().fg(t.text_muted),
        );
    }
}

// ── compact: the io box ─────────────────────────────────────────────────────

/// disk and net on one row each, every pairing preserved as a sparkline pair.
fn io_box(buf: &mut Buffer, area: Rect, t: &Theme, r: &Ramps, app: &App, snap: &Snapshot) {
    let inner = paint::panel(
        buf,
        area,
        t,
        &PanelOpts {
            key: Some("2"),
            title: Some("io"),
            sub: Some("disk · net"),
            foot_right: Some("r/w · down/up"),
            ..Default::default()
        },
    );
    if inner.height < 2 || inner.width < 40 {
        return;
    }
    // Two blocks of `label + value + spark` share the row, so each spark
    // gets half of whatever the fixed-width parts leave.
    let sw = (inner.width.saturating_sub(34) / 2).clamp(4, 24);
    let rows: [(&str, &str, &str, &str); 2] = [("▪", "disk", "r", "w"), ("▪", "net", "↓", "↑")];
    for (i, (glyph, name, a_lab, b_lab)) in rows.iter().enumerate() {
        let y = inner.y + i as u16;
        if y >= inner.bottom() {
            break;
        }
        let (a, b) = if i == 0 {
            (
                Series::rates(app.history.io_read.iter().copied(), sw),
                Series::rates(app.history.io_write.iter().copied(), sw),
            )
        } else {
            (
                Series::rates(app.history.net_rx.iter().copied(), sw),
                Series::rates(app.history.net_tx.iter().copied(), sw),
            )
        };
        let slot = if i == 0 {
            &app.dense.io_ceiling
        } else {
            &app.dense.net_ceiling
        };
        let max = ease_ceiling(slot, nice_ceil(a.peak.max(b.peak)));
        paint::put(buf, inner.x + 1, y, glyph, Style::default().fg(t.border));
        paint::put(
            buf,
            inner.x + 3,
            y,
            name,
            Style::default().fg(t.text_secondary),
        );
        let mut x = inner.x + 9;
        for (lab, s, ramp) in [(a_lab, &a, &r.primary), (b_lab, &b, &r.secondary)] {
            paint::put(buf, x, y, lab, Style::default().fg(t.text_muted));
            paint::put_right(
                buf,
                x + 9,
                y,
                &rate_short(s.cur),
                Style::default().fg(ramp.mid()),
            );
            paint::spark(buf, x + 11, y, sw, &s.plot(), max, ramp);
            x += 12 + sw;
        }
    }
    let _ = snap;
}

// ── box 6: procs ────────────────────────────────────────────────────────────

/// Column plan for the process table, computed from the interior width.
///
/// Fields are dropped from the least load-bearing end as the terminal narrows,
/// and the history sparkline takes whatever is left — so the table degrades by
/// losing columns rather than by letting two of them share a range.
struct ProcCols {
    name: u16,
    pid: Option<u16>,
    user: Option<u16>,
    cpu: u16,
    mem: u16,
    thr: Option<u16>,
    state: Option<u16>,
    io: Option<u16>,
    spark_x: u16,
    spark_w: u16,
}

impl ProcCols {
    fn new(inner: Rect) -> Self {
        // One column of air after the row marker, or `▶` butts the name.
        let x0 = inner.x + 3;
        let mut x = x0 + 15; // name
        let w = inner.width;
        let pid = (w >= 60).then(|| {
            let c = x;
            x += 7;
            c
        });
        let user = (w >= 74).then(|| {
            let c = x;
            x += 10;
            c
        });
        let cpu = x;
        x += 8;
        let mem = x;
        x += 9;
        let thr = (w >= 88).then(|| {
            let c = x;
            x += 6;
            c
        });
        let state = (w >= 94).then(|| {
            let c = x;
            x += 4;
            c
        });
        let io = (w >= 112).then(|| {
            let c = x;
            x += 14;
            c
        });
        let spark_x = x + 1;
        let spark_w = inner.right().saturating_sub(spark_x + 1).min(28);
        Self {
            name: x0,
            pid,
            user,
            cpu,
            mem,
            thr,
            state,
            io,
            spark_x,
            spark_w,
        }
    }
}

/// The muted token to use for a row, given whether that row is selected.
///
/// A selection tint and the muted text colour can be the *same* colour: on the
/// `terminal` theme `selection_bg` is `Indexed(8)` and `text_muted` is
/// `DarkGray`, which every terminal renders identically — so every dim field
/// on the selected row vanishes. Lift dim text one rung wherever it sits on
/// the tint. Harmless on themes where the two already differ.
fn muted_on(sel: bool, t: &Theme) -> Color {
    if sel {
        t.text_secondary
    } else {
        t.text_muted
    }
}

fn procs_box(
    buf: &mut Buffer,
    area: Rect,
    t: &Theme,
    r: &Ramps,
    app: &App,
    snap: &Snapshot,
    l: &Layout,
) {
    // The border claims a sort, so sort here rather than trusting the
    // collector's order.
    let procs = sorted_procs(snap, &app.history, &app.dense);
    let visible = l.visible_procs().max(1);
    let sel = selected_index(&app.dense, &procs);
    // Scroll the window so the selection stays inside it.
    let first = sel.saturating_sub(visible.saturating_sub(1));
    let shown: Vec<&&ProcTick> = procs.iter().skip(first).take(visible).collect();

    let sub = format!("{}", procs.len());
    let range = format!("{}-{} of {}", first + 1, first + shown.len(), procs.len());
    let binds: &[Bind] = &[
        ("q", "uit"),
        ("↑↓", " select"),
        ("V", " view"),
        (",", " menu"),
        if l.zoom.is_some() {
            ("esc", " back")
        } else {
            ("1-6", " zoom")
        },
        ("?", " keys"),
    ];
    let inner = paint::panel(
        buf,
        area,
        t,
        &PanelOpts {
            // Compact shows three boxes, so procs is the third — the bracketed
            // hotkeys have to match what is actually on screen.
            key: Some(if l.shape == Shape::Compact { "3" } else { "6" }),
            title: Some("procs"),
            sub: Some(&sub),
            right: Some("sort ↓ cpu"),
            foot_left: binds,
            foot_right: Some(&range),
            ..Default::default()
        },
    );
    if inner.height < 2 || inner.width < 40 {
        return;
    }
    let cols = ProcCols::new(inner);

    // Detail-in-place: the selected row's detail is hoisted into the top of
    // this same box. No new screen, no back button.
    let mut y = inner.y;
    if inner.height >= 4 {
        if let Some(p) = procs.get(sel) {
            detail_rows(buf, inner, y, t, r, app, p, &cols);
            y += 2;
        }
    }

    // header
    let hs = Style::default().fg(t.border);
    paint::put(buf, cols.name, y, "PROCESS", hs);
    if let Some(x) = cols.pid {
        paint::put_right(buf, x + 5, y, "PID", hs);
    }
    if let Some(x) = cols.user {
        paint::put(buf, x, y, "USER", hs);
    }
    paint::put_right(
        buf,
        cols.cpu + 5,
        y,
        "CPU%",
        Style::default().fg(t.brand).add_modifier(Modifier::BOLD),
    );
    paint::put_right(buf, cols.mem + 6, y, "MEM", hs);
    if let Some(x) = cols.thr {
        paint::put_right(buf, x + 3, y, "THR", hs);
    }
    if let Some(x) = cols.state {
        paint::put(buf, x, y, "ST", hs);
    }
    if let Some(x) = cols.io {
        paint::put_right(buf, x + 11, y, "IO R/W", hs);
    }
    if cols.spark_w >= 6 {
        paint::put(buf, cols.spark_x, y, "60s", hs);
    }
    y += 1;

    for (i, p) in shown.iter().enumerate() {
        let row_y = y + i as u16;
        if row_y >= inner.bottom() {
            break;
        }
        let is_sel = first + i == sel;
        if is_sel {
            for x in inner.x..inner.right() {
                if let Some(cell) = buf.cell_mut((x, row_y)) {
                    cell.set_bg(t.selection_bg);
                }
            }
        }
        let idle = p.cpu_pct < 2.0;
        let busy = p.cpu_pct > 50.0;
        paint::put(
            buf,
            inner.x + 1,
            row_y,
            if is_sel { "▶" } else { "●" },
            Style::default().fg(if is_sel {
                t.brand
            } else if idle {
                t.border
            } else if busy {
                t.status_warn
            } else {
                t.status_good
            }),
        );
        paint::put(
            buf,
            cols.name,
            row_y,
            &truncate(&p.name, 14),
            Style::default().fg(if idle {
                muted_on(is_sel, t)
            } else {
                t.text_primary
            }),
        );
        if let Some(x) = cols.pid {
            paint::put_right(
                buf,
                x + 5,
                row_y,
                &p.pid.to_string(),
                Style::default().fg(muted_on(is_sel, t)),
            );
        }
        if let Some(x) = cols.user {
            paint::put(
                buf,
                x,
                row_y,
                &truncate(&p.user, 8),
                Style::default().fg(muted_on(is_sel, t)),
            );
        }
        paint::put_right(
            buf,
            cols.cpu + 5,
            row_y,
            &format!("{:.1}", p.cpu_pct),
            Style::default().fg(if busy {
                t.status_warn
            } else if idle {
                muted_on(is_sel, t)
            } else {
                t.status_good
            }),
        );
        paint::put_right(
            buf,
            cols.mem + 6,
            row_y,
            &human_bytes(p.mem_rss),
            // Memory reads in the secondary accent, matching the mem box's own
            // graph. `tx_rate` is netwatch's magenta upload token and has no
            // business colouring RSS.
            Style::default().fg(r.secondary.mid()),
        );
        if let Some(x) = cols.thr {
            paint::put_right(
                buf,
                x + 3,
                row_y,
                &p.threads.map(|n| n.to_string()).unwrap_or("--".into()),
                Style::default().fg(muted_on(is_sel, t)),
            );
        }
        if let Some(x) = cols.state {
            paint::put(
                buf,
                x,
                row_y,
                &p.state.to_string(),
                Style::default().fg(match p.state {
                    'R' => t.status_good,
                    'D' => t.status_error,
                    _ => muted_on(is_sel, t),
                }),
            );
        }
        if let Some(x) = cols.io {
            paint::put_right(
                buf,
                x + 11,
                row_y,
                &format!(
                    "{}/{}",
                    rate_short(p.io_read_rate as u64),
                    rate_short(p.io_write_rate as u64)
                ),
                Style::default().fg(muted_on(is_sel, t)),
            );
        }
        if cols.spark_w >= 6 {
            match app.history.proc_cpu_history.get(&p.pid) {
                Some(ring) if ring.len() > 1 => {
                    let vals: Vec<u64> = ring.iter().map(|&v| paint::perceptual(v, 64)).collect();
                    let s = Series::fixed(vals, cols.spark_w, 64);
                    paint::spark(
                        buf,
                        cols.spark_x,
                        row_y,
                        cols.spark_w,
                        &s.plot(),
                        s.max,
                        &r.primary,
                    );
                }
                _ => paint::baseline(buf, cols.spark_x, row_y, cols.spark_w, r.dim.mid()),
            }
        }
    }
}

/// The two-row detail block. Fields are laid out left-to-right with a fixed
/// gutter and measured against the right-hand note before anything is drawn,
/// so a long process name can never collide with the field after it.
#[allow(clippy::too_many_arguments)]
fn detail_rows(
    buf: &mut Buffer,
    inner: Rect,
    y: u16,
    t: &Theme,
    r: &Ramps,
    app: &App,
    p: &ProcTick,
    cols: &ProcCols,
) {
    // **No tint here.** Exactly one thing in this box is highlighted: the
    // active process row. Tinting the detail block as well put a second filled
    // band above the column header, so the header read as highlighted chrome
    // sitting between two selections. The `↳` in the accent colour is what
    // binds this block to the row it describes.
    paint::put(buf, inner.x + 1, y, "↳", Style::default().fg(t.brand));

    let note = format!("nice --  {} threads", p.threads.unwrap_or(0));
    let segs: Vec<(String, Style)> = vec![
        (
            truncate(&p.name, 14),
            Style::default()
                .fg(t.text_primary)
                .add_modifier(Modifier::BOLD),
        ),
        (format!("pid {}", p.pid), Style::default().fg(t.text_muted)),
        (
            format!("ppid {}", p.ppid),
            Style::default().fg(t.text_muted),
        ),
        (
            format!("state {}", p.state),
            Style::default().fg(if p.state == 'D' {
                t.status_error
            } else {
                t.status_good
            }),
        ),
        (
            format!("rss {}", human_bytes(p.mem_rss)),
            Style::default().fg(r.secondary.mid()),
        ),
        (
            format!("virt {}", human_bytes(p.mem_virt)),
            Style::default().fg(t.text_muted),
        ),
    ];
    row_with_note(buf, inner, y, inner.x + 3, &segs, &note, t);

    // The selection's own cpu strip sits in the SAME column as the table's
    // `60s` field directly below it, so the detail block reads as the selected
    // row pulled up rather than as a second, differently-shaped thing. The
    // command line takes the width left over to its left.
    let y2 = y + 1;
    if y2 >= inner.bottom() {
        return;
    }
    paint::put(
        buf,
        inner.x + 3,
        y2,
        "cpu",
        Style::default().fg(t.text_muted),
    );
    if cols.spark_w >= 4 {
        match app.history.proc_cpu_history.get(&p.pid) {
            Some(ring) if ring.len() > 1 => {
                let vals: Vec<u64> = ring.iter().map(|&v| paint::perceptual(v, 64)).collect();
                let s = Series::fixed(vals, cols.spark_w, 64);
                paint::spark(
                    buf,
                    cols.spark_x,
                    y2,
                    cols.spark_w,
                    &s.plot(),
                    s.max,
                    &r.primary,
                );
            }
            _ => paint::baseline(buf, cols.spark_x, y2, cols.spark_w, r.dim.mid()),
        }
    }
    let room = cols.spark_x.saturating_sub(inner.x + 9) as usize;
    if room > 4 && !p.cmd.is_empty() {
        paint::put(
            buf,
            inner.x + 7,
            y2,
            &truncate(&short_cmd(&p.cmd), room),
            Style::default().fg(t.text_muted),
        );
    }
}

/// Lay `segs` left-to-right with a two-column gutter, then right-align `note`.
/// The two runs are measured against each other before drawing: segments that
/// would collide with the note are dropped, so the row degrades by showing
/// fewer fields rather than by overwriting one with another.
fn row_with_note(
    buf: &mut Buffer,
    inner: Rect,
    y: u16,
    x0: u16,
    segs: &[(String, Style)],
    note: &str,
    t: &Theme,
) {
    let note_w = note.width() as u16;
    let budget = inner.right().saturating_sub(1).saturating_sub(note_w + 2);
    let mut x = x0;
    for (s, style) in segs {
        let w = s.width() as u16;
        if x + w > budget {
            break;
        }
        paint::put(buf, x, y, s, *style);
        x += w + 2;
    }
    paint::put_right(
        buf,
        inner.right() - 2,
        y,
        note,
        Style::default().fg(t.text_muted),
    );
}

// ── small helpers ───────────────────────────────────────────────────────────

/// A right-aligned axis label plus its tick, both anchored on the shared
/// gutter constants so every graph's left edge lines up.
///
/// The gutter is `AX_TICK` columns wide and the label must fit it. Anything
/// longer is dropped by `put_right` rather than overrunning the graph, so axis
/// labels use [`bytes_axis`] / [`rate_short`] rather than the full
/// `human_bytes` spelling — a silently missing ceiling is worse than a terse
/// one.
fn tick(buf: &mut Buffer, ix: u16, y: u16, label: &str, t: &Theme) {
    tick_styled(buf, ix, y, label, t, Style::default().fg(t.border));
}

/// Bytes at axis width: `32G`, not `32.0 GB`. Must fit `AX_TICK` columns.
fn bytes_axis(b: u64) -> String {
    const K: u64 = 1024;
    match b {
        v if v >= K * K * K * K => format!("{}T", v / (K * K * K * K)),
        v if v >= K * K * K => format!("{}G", v / (K * K * K)),
        v if v >= K * K => format!("{}M", v / (K * K)),
        v if v >= K => format!("{}K", v / K),
        v => format!("{v}"),
    }
}

fn tick_styled(buf: &mut Buffer, ix: u16, y: u16, label: &str, t: &Theme, style: Style) {
    paint::put_right(buf, ix + AX_TICK - 1, y, label, style);
    paint::put(buf, ix + AX_TICK, y, "┤", Style::default().fg(t.border));
}

/// Bytes/sec in the narrowest honest form. Stable width matters more than
/// precision here: a column that jitters as values change is unreadable.
fn rate_short(bps: u64) -> String {
    const K: u64 = 1024;
    match bps {
        b if b >= K * K * K => format!("{:.1}G", b as f64 / (K * K * K) as f64),
        b if b >= 100 * K * K => format!("{:.0}M", b as f64 / (K * K) as f64),
        b if b >= K * K => format!("{:.1}M", b as f64 / (K * K) as f64),
        b if b >= 100 * K => format!("{:.0}K", b as f64 / K as f64),
        b if b >= K => format!("{:.1}K", b as f64 / K as f64),
        b => format!("{b}B"),
    }
}

/// How much wall-clock the visible window covers, from the session ring's own
/// timestamps rather than an assumed tick interval.
fn window_secs(h: &History, graph_w: u16) -> u64 {
    let n = (graph_w as usize * 2).min(h.session.len());
    if n < 2 {
        return 0;
    }
    let newest = h.session.nth_back(0).map(|s| s.t);
    let oldest = h.session.nth_back(n - 1).map(|s| s.t);
    match (newest, oldest) {
        (Some(a), Some(b)) => a.duration_since(b).map(|d| d.as_secs()).unwrap_or(0),
        _ => 0,
    }
}

fn fmt_secs(s: u64) -> String {
    match s {
        0 => "--".into(),
        s if s < 90 => format!("{s}s"),
        s if s < 5400 => format!("{}m", s / 60),
        s => format!("{}h", s / 3600),
    }
}

fn fmt_uptime(secs: u64) -> String {
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3600;
    let m = (secs % 3600) / 60;
    if d > 0 {
        format!("{d}d {h:02}:{m:02}")
    } else {
        format!("{h:02}:{m:02}")
    }
}

/// Drop the marketing noise Apple and Intel put in `cpu_model` so the sub-label
/// is the chip, not a paragraph.
fn trim_cpu_model(m: &str) -> &str {
    let m = m.trim();
    if m.is_empty() {
        return "cpu";
    }
    m.split(" with ").next().unwrap_or(m).trim()
}

/// Trim a command line down to the part that identifies it.
///
/// Clipping the head off a long absolute path keeps the least useful end:
/// `/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/c` tells
/// you nothing that the next twenty processes don't also say. Reduce argv[0] to
/// its basename and keep the arguments, which is what actually distinguishes
/// one invocation from another.
fn short_cmd(cmd: &str) -> String {
    let mut parts = cmd.split_whitespace();
    let Some(argv0) = parts.next() else {
        return String::new();
    };
    let base = argv0.rsplit('/').next().unwrap_or(argv0);
    let rest: Vec<&str> = parts.collect();
    if rest.is_empty() {
        base.to_string()
    } else {
        format!("{} {}", base, rest.join(" "))
    }
}

/// Clamp to `max` **display columns**, not bytes or chars — a wide glyph in a
/// process name would otherwise shear every column to its right.
fn truncate(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = c.to_string().width();
        if w + cw > max {
            break;
        }
        out.push(c);
        w += cw;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::TabId;
    use crate::app::ViewMode;
    use crate::collect::MemTick;
    use crate::config::SyswatchConfig;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn snap() -> Snapshot {
        let mut s = Snapshot::default();
        s.host.hostname = "testbox".into();
        s.host.cpu_model = "Apple M3 Pro".into();
        s.host.uptime_secs = 4 * 86_400 + 2 * 3600 + 18 * 60;
        s.cpu.usage_pct = 24.8;
        s.cpu.load_1 = 1.42;
        s.cpu.per_core = vec![46.0, 42.0, 48.0, 28.0, 18.0, 12.0, 12.0, 16.0];
        s.mem = MemTick {
            total_bytes: 32 * 1024 * 1024 * 1024,
            used_bytes: 19 * 1024 * 1024 * 1024,
            available_bytes: 13 * 1024 * 1024 * 1024,
            swap_total_bytes: 4 * 1024 * 1024 * 1024,
            swap_used_bytes: 0,
            pressure_level: None,
        };
        s.disk_io.read_rate = 128.0 * 1024.0 * 1024.0;
        s.disk_io.write_rate = 14.0 * 1024.0 * 1024.0;
        s.procs = (0..12)
            .map(|i| ProcTick {
                pid: 100 + i,
                ppid: 1,
                name: format!("proc{i}"),
                user: "matt".into(),
                cpu_pct: 90.0 - i as f32 * 7.0,
                mem_rss: (100 - i as u64) * 1024 * 1024,
                threads: Some(4),
                state: 'S',
                ..Default::default()
            })
            .collect();
        s
    }

    fn app_with_history() -> App {
        let mut app = App::new(TabId::Overview, SyswatchConfig::default());
        app.view_mode = ViewMode::Dense;
        let s = snap();
        for _ in 0..80 {
            app.history.push(&s);
        }
        app.snap = Some(s);
        app
    }

    fn render_at(w: u16, h: u16) -> ratatui::buffer::Buffer {
        let _g = theme::exclusive_theme();
        let app = app_with_history();
        let s = app.snap.clone().unwrap();
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| render(f, &app, &s)).unwrap();
        term.backend().buffer().clone()
    }

    fn row(b: &ratatui::buffer::Buffer, y: u16) -> String {
        (0..b.area.width)
            .map(|x| b[(x, y)].symbol().chars().next().unwrap_or(' '))
            .collect()
    }

    #[test]
    fn reference_size_uses_the_six_box_layout() {
        let l = Layout::reference();
        assert_eq!(l.shape, Shape::Full);
        assert_eq!(l.cpu, Rect::new(0, 0, 130, 12));
        assert_eq!(l.mem.y, 12);
        assert_eq!(l.net.x, 65);
        assert_eq!(l.cores.y, 24);
        assert_eq!(l.disk.y, 24);
        assert_eq!(l.procs.y, 32);
        // Every row is spoken for — that is the "zero chrome rows" property.
        assert_eq!(l.procs.bottom(), GRID_H);
    }

    /// The bands must tile with no gap and no overlap at any size, or a box
    /// will paint over its neighbour's border.
    #[test]
    fn bands_tile_without_gaps_at_many_sizes() {
        for w in [80u16, 100, 110, 130, 200, 300] {
            for h in [24u16, 30, 37, 38, 44, 60, 100] {
                let l = Layout::new(Rect::new(0, 0, w, h));
                assert_eq!(l.cpu.y, 0, "{w}x{h}");
                match l.shape {
                    Shape::Full => {
                        assert_eq!(l.cpu.bottom(), l.mem.y, "{w}x{h}");
                        assert_eq!(l.mem.bottom(), l.cores.y, "{w}x{h}");
                        assert_eq!(l.cores.bottom(), l.procs.y, "{w}x{h}");
                        assert_eq!(l.mem.right(), l.net.x, "{w}x{h}");
                        assert_eq!(l.cores.right(), l.disk.x, "{w}x{h}");
                        assert_eq!(l.net.right(), w, "{w}x{h}");
                    }
                    Shape::Compact => {
                        assert_eq!(l.cpu.bottom(), l.mem.y, "{w}x{h}");
                        assert_eq!(l.mem.bottom(), l.procs.y, "{w}x{h}");
                    }
                }
                assert_eq!(l.procs.bottom(), h, "{w}x{h}");
            }
        }
    }

    /// A zoomed box owns the whole frame and every other box draws nothing.
    #[test]
    fn zoom_gives_one_box_the_whole_frame() {
        let area = Rect::new(0, 0, GRID_W, GRID_H);
        for id in [1u8, 2, 3, 4, 5, 6] {
            let l = Layout::with_zoom(area, Some(id));
            let rects = [l.cpu, l.mem, l.net, l.cores, l.disk, l.procs];
            let full: Vec<Rect> = rects.iter().copied().filter(|r| r.width > 0).collect();
            assert_eq!(full.len(), 1, "box {id}: expected exactly one live rect");
            assert_eq!(full[0], area, "box {id} should fill the frame");
        }
        // No zoom, or a box this shape doesn't have, is the plain grid.
        assert_eq!(Layout::with_zoom(area, None).cpu.height, H_CPU);
        let compact = Rect::new(0, 0, 80, 24);
        assert!(Layout::with_zoom(compact, Some(5)).zoom.is_none());
    }

    /// Zooming the process table must actually buy rows.
    #[test]
    fn zoomed_procs_shows_more_rows_than_the_grid() {
        let area = Rect::new(0, 0, GRID_W, GRID_H);
        let grid = Layout::new(area).visible_procs();
        let zoomed = Layout::with_zoom(area, Some(6)).visible_procs();
        assert!(zoomed > grid * 3, "grid {grid}, zoomed {zoomed}");
    }

    #[test]
    fn small_terminals_fall_back_to_compact() {
        assert_eq!(Layout::new(Rect::new(0, 0, 80, 24)).shape, Shape::Compact);
        assert_eq!(Layout::new(Rect::new(0, 0, 130, 30)).shape, Shape::Compact);
        assert_eq!(Layout::new(Rect::new(0, 0, 90, 44)).shape, Shape::Compact);
        assert_eq!(Layout::new(Rect::new(0, 0, 130, 44)).shape, Shape::Full);
    }

    #[test]
    fn nice_ceil_never_wastes_more_than_a_fifth_of_the_graph() {
        let mut worst = 0.0f64;
        let mut v = 1u64;
        while v < 5_000_000_000 {
            let c = nice_ceil(v);
            assert!(c >= v, "ceiling {c} below peak {v}");
            worst = worst.max(1.0 - v as f64 / c as f64);
            v = (v as f64 * 1.017) as u64 + 1;
        }
        assert!(
            worst <= 0.21,
            "worst-case unused height {:.1}%",
            worst * 100.0
        );
    }

    /// The headline, the peak and the pixels must all come from the same
    /// window — that is the whole point of `Series`.
    #[test]
    fn series_stats_describe_only_the_visible_window() {
        // A 2-column graph holds 2 * SAMPLES_PER_COL samples; anything older
        // has scrolled off and must not colour the numbers printed beside it.
        let n = 2 * SAMPLES_PER_COL;
        let mut all = vec![9999u64];
        all.extend((1..=n as u64).map(|v| v));
        let s = Series::fixed(all, 2, 100);
        assert_eq!(s.vals.len(), n);
        assert_eq!(
            s.peak, n as u64,
            "peak must not come from scrolled-off history"
        );
        assert_eq!(s.cur, n as u64);
        // Whatever the density, the plot always fills both sub-columns.
        assert_eq!(s.plot().len(), 4);
    }

    /// A graph must be able to fill its full width from the ring behind it.
    /// The cpu graph wanted 240 samples from a 120-deep ring, so its left half
    /// was blank permanently — no amount of waiting could fill it.
    #[test]
    fn the_history_ring_can_fill_the_widest_graph() {
        let l = Layout::reference();
        let widest = l.cpu.width - 2 - AX_GRAPH - 1;
        let needed = widest as usize * SAMPLES_PER_COL;
        let h = History::new(120);
        // Push more than the ring can hold, then check it still yields enough.
        let sn = Snapshot::default();
        let mut h = h;
        for _ in 0..(needed + 50) {
            h.push(&sn);
        }
        assert!(
            h.cpu.len() >= needed,
            "ring holds {} samples, the cpu graph needs {needed}",
            h.cpu.len()
        );
    }

    #[test]
    fn renders_at_reference_size_without_panicking() {
        let b = render_at(GRID_W, GRID_H);
        let top = row(&b, 0);
        assert!(top.starts_with("╭─┤1├─┤ cpu ├"), "got {top:?}");
        assert!(
            top.contains("testbox"),
            "host should be in the border: {top:?}"
        );
        // Zero chrome rows: the last row is the procs box's own border.
        let last = row(&b, GRID_H - 1);
        assert!(last.starts_with('╰') && last.ends_with('╯'), "got {last:?}");
        assert!(
            last.contains("uit"),
            "keybinds live in the border: {last:?}"
        );
    }

    /// A box's title row must not be overwritten by the box beside it.
    #[test]
    fn side_by_side_boxes_keep_their_own_borders() {
        let b = render_at(GRID_W, GRID_H);
        let mid = row(&b, 12);
        assert!(mid.contains("┤ mem ├"), "got {mid:?}");
        assert!(mid.contains("┤ net ├"), "got {mid:?}");
        let small = row(&b, 24);
        assert!(small.contains("┤ cores ├"), "got {small:?}");
        assert!(small.contains("┤ disk ├"), "got {small:?}");
    }

    #[test]
    fn compact_renders_the_io_box() {
        let b = render_at(80, 24);
        let all: String = (0..24).map(|y| row(&b, y)).collect::<Vec<_>>().join("\n");
        assert!(all.contains("┤ io ├"), "got:\n{all}");
        assert!(all.contains("┤ cpu ├"));
        assert!(all.contains("┤ procs ├"));
    }

    /// Every size the layout claims to support must actually draw.
    #[test]
    fn renders_across_sizes_without_panicking() {
        for (w, h) in [
            (60u16, 20u16),
            (80, 24),
            (100, 30),
            (110, 38),
            (130, 44),
            (200, 60),
            (400, 100),
        ] {
            let _ = render_at(w, h);
        }
    }

    /// Exactly one thing in the procs box is highlighted: the active process.
    ///
    /// The detail block used to carry the selection tint too, which put a
    /// second filled band directly above the column header — so the header
    /// read as highlighted chrome sandwiched between two selections.
    #[test]
    fn only_the_active_process_row_is_highlighted() {
        let _g = theme::exclusive_theme();
        theme::set_by_name("dark");
        let t = theme::active();
        let app = app_with_history();
        let sn = app.snap.clone().unwrap();
        let mut term = Terminal::new(TestBackend::new(GRID_W, GRID_H)).unwrap();
        term.draw(|f| render(f, &app, &sn)).unwrap();
        let b = term.backend().buffer().clone();

        let tinted: Vec<u16> = (0..GRID_H)
            .filter(|&y| (0..GRID_W).any(|x| b[(x, y)].bg == t.selection_bg))
            .collect();
        assert_eq!(
            tinted.len(),
            1,
            "expected one highlighted row, got rows {tinted:?}"
        );

        // And it is the selected process row, not the header or the detail.
        let l = Layout::new(Rect::new(0, 0, GRID_W, GRID_H));
        let row = tinted[0];
        let text: String = (0..GRID_W)
            .map(|x| b[(x, row)].symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(
            text.contains('▶'),
            "highlighted row is not the cursor row: {text:?}"
        );
        assert!(
            !text.contains("PROCESS"),
            "the column header must not be tinted"
        );
        assert!(!text.contains('↳'), "the detail block must not be tinted");
        assert!(
            row >= l.procs.y + 4,
            "highlight landed above the table body"
        );
    }

    #[test]
    fn procs_table_is_sorted_by_cpu_descending() {
        let b = render_at(GRID_W, GRID_H);
        // The first data row of the procs box: box starts row 32, +1 border,
        // +2 detail, +1 header.
        let first = row(&b, 36);
        assert!(
            first.contains("proc0"),
            "highest-cpu proc should lead: {first:?}"
        );
    }

    #[test]
    fn a_terminal_too_small_for_even_compact_says_so() {
        let b = render_at(40, 10);
        let all: String = (0..10).map(|y| row(&b, y)).collect::<Vec<_>>().join(" ");
        assert!(all.contains("dense wants"), "got {all:?}");
        // And nothing else — no half-drawn boxes behind the notice.
        assert!(!all.contains('╭'), "got {all:?}");
    }

    /// Nothing may be drawn in a colour equal to the background it sits on.
    ///
    /// This is not hypothetical: on the `terminal` theme `selection_bg` is
    /// `Indexed(8)` and `text_muted` is `DarkGray` — the same colour in every
    /// terminal — so every dim field on the selected row and in the detail
    /// block rendered invisible. Checked across all themes because the clash
    /// depends entirely on which pair of tokens a theme happens to pick.
    #[test]
    fn no_cell_is_drawn_invisible_against_its_own_background() {
        let _g = theme::exclusive_theme();
        for name in crate::ui::theme::THEME_NAMES {
            theme::set_by_name(name);
            let app = app_with_history();
            let sn = app.snap.clone().unwrap();
            let mut term = Terminal::new(TestBackend::new(GRID_W, GRID_H)).unwrap();
            term.draw(|f| render(f, &app, &sn)).unwrap();
            let b = term.backend().buffer().clone();
            for y in 0..GRID_H {
                for x in 0..GRID_W {
                    let c = &b[(x, y)];
                    if c.symbol().trim().is_empty() {
                        continue;
                    }
                    assert!(
                        !same_ink(c.fg, c.bg),
                        "{name}: {:?} at {x},{y} is invisible — fg {:?} == bg {:?}",
                        c.symbol(),
                        c.fg,
                        c.bg
                    );
                }
            }
        }
    }

    /// `DarkGray` and `Indexed(8)` are the same ANSI slot; so are `Gray` and
    /// `Indexed(7)`, and each named colour and its index.
    fn same_ink(a: Color, b: Color) -> bool {
        fn slot(c: Color) -> Option<u8> {
            Some(match c {
                Color::Black => 0,
                Color::Red => 1,
                Color::Green => 2,
                Color::Yellow => 3,
                Color::Blue => 4,
                Color::Magenta => 5,
                Color::Cyan => 6,
                Color::Gray => 7,
                Color::DarkGray => 8,
                Color::LightRed => 9,
                Color::LightGreen => 10,
                Color::LightYellow => 11,
                Color::LightBlue => 12,
                Color::LightMagenta => 13,
                Color::LightCyan => 14,
                Color::White => 15,
                Color::Indexed(n) => n,
                _ => return None,
            })
        }
        match (a, b) {
            (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => (r1, g1, b1) == (r2, g2, b2),
            _ => match (slot(a), slot(b)) {
                (Some(x), Some(y)) => x == y,
                _ => false,
            },
        }
    }

    /// The table must not read as a slot machine.
    ///
    /// On a busy machine half a dozen processes sit permanently within noise of
    /// each other; sorting on the raw per-tick CPU figure makes them trade
    /// places every single second. Measured here against the same input, so the
    /// comparison is real rather than two different live captures.
    #[test]
    fn banded_order_is_far_stabler_than_raw_cpu_sort() {
        let base = [40.0f32, 12.0, 11.6, 11.2, 10.8, 10.4, 10.0, 5.0];
        let mut h = History::new(64);
        let st = DenseState::default();
        let mut seed = 12345u64;
        let mut rnd = move || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((seed >> 33) as f32 / (u32::MAX >> 1) as f32) * 2.0 - 1.0
        };
        let (mut raw_moves, mut banded_moves) = (0usize, 0usize);
        let (mut prev_raw, mut prev_banded): (Vec<u32>, Vec<u32>) = (vec![], vec![]);
        for _ in 0..60 {
            let mut sn = Snapshot::default();
            sn.procs = base
                .iter()
                .enumerate()
                .map(|(i, &b)| ProcTick {
                    pid: 100 + i as u32,
                    name: format!("p{i}"),
                    cpu_pct: (b + rnd() * 2.0).max(0.0),
                    ..Default::default()
                })
                .collect();
            h.push(&sn);
            let mut raw: Vec<&ProcTick> = sn.procs.iter().collect();
            raw.sort_by(|a, b| b.cpu_pct.partial_cmp(&a.cpu_pct).unwrap());
            let raw_ids: Vec<u32> = raw.iter().map(|p| p.pid).collect();
            let banded_ids: Vec<u32> = sorted_procs(&sn, &h, &st).iter().map(|p| p.pid).collect();
            if !prev_raw.is_empty() {
                raw_moves += prev_raw
                    .iter()
                    .zip(&raw_ids)
                    .filter(|(a, b)| a != b)
                    .count();
                banded_moves += prev_banded
                    .iter()
                    .zip(&banded_ids)
                    .filter(|(a, b)| a != b)
                    .count();
            }
            prev_raw = raw_ids;
            prev_banded = banded_ids;
        }
        assert!(
            banded_moves * 4 < raw_moves,
            "banded {banded_moves} vs raw {raw_moves} — hysteresis is not earning its keep"
        );
    }

    /// Stability must not become stickiness: a process that genuinely takes off
    /// has to reach the top promptly.
    #[test]
    fn a_real_load_change_still_reorders_the_table() {
        let mut h = History::new(64);
        let st = DenseState::default();
        let mk = |busy: f32| {
            let mut sn = Snapshot::default();
            sn.procs = (0..5)
                .map(|i| ProcTick {
                    pid: 100 + i,
                    name: format!("p{i}"),
                    cpu_pct: if i == 4 { busy } else { 10.0 },
                    ..Default::default()
                })
                .collect();
            sn
        };
        for _ in 0..10 {
            let sn = mk(1.0);
            h.push(&sn);
            sorted_procs(&sn, &h, &st);
        }
        assert_ne!(sorted_procs(&mk(1.0), &h, &st)[0].pid, 104, "idle proc led");
        // Now it goes hot.
        for _ in 0..6 {
            let sn = mk(95.0);
            h.push(&sn);
            sorted_procs(&sn, &h, &st);
        }
        let sn = mk(95.0);
        assert_eq!(
            sorted_procs(&sn, &h, &st)[0].pid,
            104,
            "a process at 95% must lead the table"
        );
    }

    /// The cursor follows the *process*, not the row number.
    #[test]
    fn selection_sticks_to_its_process_when_rows_reorder() {
        let a = ProcTick {
            pid: 1,
            cpu_pct: 50.0,
            ..Default::default()
        };
        let b = ProcTick {
            pid: 2,
            cpu_pct: 40.0,
            ..Default::default()
        };
        let before = [&a, &b];
        let after = [&b, &a]; // they swapped
        let mut st = DenseState::default();
        st.selected = 1;
        st.selected_pid = Some(2);
        assert_eq!(selected_index(&st, &before), 1);
        assert_eq!(
            selected_index(&st, &after),
            0,
            "cursor should have followed pid 2 to its new row"
        );
        // A process that exits falls back to the remembered row.
        st.selected_pid = Some(999);
        assert_eq!(selected_index(&st, &after), 1);
    }

    /// Ceilings rise at once and fall slowly. A ceiling taken straight from the
    /// visible peak collapsed the trace to the baseline the moment a burst
    /// scrolled out of the window.
    #[test]
    fn graph_ceiling_rises_at_once_and_falls_gradually() {
        let slot = Cell::new(0u64);
        assert_eq!(
            ease_ceiling(&slot, 1000),
            1000,
            "first value adopts exactly"
        );
        assert_eq!(
            ease_ceiling(&slot, 500_000),
            500_000,
            "a spike must not clip"
        );
        let a = ease_ceiling(&slot, 1000);
        assert!(a < 500_000 && a > 1000, "should ease down, got {a}");
        let mut prev = a;
        for _ in 0..400 {
            let n = ease_ceiling(&slot, 1000);
            assert!(n <= prev, "must be monotonic on the way down");
            prev = n;
        }
        assert_eq!(prev, 1000, "must actually arrive, not asymptote");
    }

    #[test]
    fn truncate_counts_display_columns() {
        assert_eq!(truncate("abcdef", 3), "abc");
        assert_eq!(truncate("日本語です", 4), "日本");
    }

    /// `thermal_throttle_pct` is a speed LIMIT: 100 means unthrottled. Reading
    /// it as "amount of throttling" reported every idle Mac as thermally
    /// throttled, which is the most alarming thing the vitals row can say.
    #[test]
    fn throttle_verdict_reads_the_speed_limit_the_right_way_round() {
        let _g = theme::exclusive_theme();
        let render_with = |pct: Option<u32>| {
            let mut app = app_with_history();
            let mut s = app.snap.clone().unwrap();
            s.power.thermal_throttle_pct = pct;
            app.snap = Some(s.clone());
            let mut term = Terminal::new(TestBackend::new(GRID_W, GRID_H)).unwrap();
            term.draw(|f| render(f, &app, &s)).unwrap();
            row(term.backend().buffer(), 10)
        };
        assert!(render_with(Some(100)).contains("throttle none"));
        assert!(render_with(Some(60)).contains("throttle ACTIVE"));
        assert!(!render_with(None).contains("throttle"));
    }

    #[test]
    fn short_cmd_keeps_the_identifying_end_of_a_path() {
        assert_eq!(
            short_cmd("/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.WebContent.xpc"),
            "com.apple.WebKit.WebContent.xpc"
        );
        assert_eq!(
            short_cmd("/usr/bin/ssh -N -L 8080:localhost:80 host"),
            "ssh -N -L 8080:localhost:80 host"
        );
        assert_eq!(short_cmd("cargo"), "cargo");
        assert_eq!(short_cmd(""), "");
    }

    #[test]
    fn rate_short_keeps_a_stable_shape() {
        assert_eq!(rate_short(0), "0B");
        assert_eq!(rate_short(2048), "2.0K");
        assert_eq!(rate_short(150 * 1024), "150K");
        assert_eq!(rate_short(5 * 1024 * 1024), "5.0M");
        assert_eq!(rate_short(200 * 1024 * 1024), "200M");
    }
}
