use std::io;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::Terminal;

use std::collections::HashMap;

pub use crate::collect::Snapshot;
use crate::collect::{Collector, Ring};
use crate::config::SyswatchConfig;
use crate::insights::{self, Insight};
use crate::tabs;
use crate::ui::chrome;
use crate::ui::graph::GraphStyle;

pub struct Options {
    pub start_tab: Option<String>,
    /// Source of truth for tick_ms / theme / graph_style / default_tab.
    /// CLI overrides are applied to this struct in main() before handoff.
    pub config: SyswatchConfig,
    /// Pre-loaded snapshots from a `--replay` invocation. When Some,
    /// the run loop skips live collection entirely and the user
    /// scrubs through the recorded ticks instead.
    pub replay: Option<Vec<Snapshot>>,
    /// Start in the Lite view (`--lite`). Opt-in only.
    pub lite: bool,
}

/// Number of CPU-usage samples kept per process for the inline Procs sparkline.
pub const PROC_CPU_SPARK_LEN: usize = 8;

pub struct History {
    /// Aggregate CPU usage % (0..100), one sample per tick.
    pub cpu: Ring<f32>,
    /// Memory used / total ratio (0..1).
    pub mem: Ring<f32>,
    /// Swap used in bytes — used by the swap-thrash heuristic.
    pub swap: Ring<u64>,
    /// Net rx+tx bytes/sec aggregated.
    pub net_rate: Ring<f64>,
    /// Disk rd+wr bytes/sec aggregated.
    pub io_rate: Ring<f64>,
    /// Aggregate GPU usage % (0..100), max across all detected devices per
    /// tick — captures the "any GPU is busy" signal regardless of which one.
    /// 0 when no GPU exposes live util (Linux NVIDIA without nvml, etc.).
    pub gpu_util: Ring<f32>,
    /// Per-device GPU util % (0..100) keyed by device name, one sample per
    /// tick. Parallel to `gpu_util` (the cross-device max) but preserves each
    /// device's own line so the GPU tab can draw a sparkline per card. Series
    /// are created lazily as devices appear and only get a sample on ticks
    /// where the device reports `util_pct` — a device that never exposes live
    /// util keeps no series, so the tab shows its honest "no live util" state
    /// instead of a fake flat-zero graph.
    pub gpu_util_by_name: HashMap<String, Ring<f32>>,
    /// Per-device VRAM used fraction (0..1) keyed by device name. Same lazy,
    /// only-when-reported discipline as `gpu_util_by_name`: a sample lands
    /// only on ticks where the device reports both a total and a used figure,
    /// so devices that don't expose VRAM usage keep no series.
    pub gpu_vram_by_name: HashMap<String, Ring<f32>>,
    /// Per-pid CPU EWMA, decayed each tick. Pids absent in the latest tick
    /// are pruned. Values are 0..100. The runaway-proc heuristic reads this
    /// to find processes whose load is sustained, not transient.
    pub proc_cpu_ewma: HashMap<u32, f32>,
    /// Per-pid short CPU-usage history (0..1, raw cpu_pct/100) for the
    /// btop-style inline sparkline in the Procs tab (issue #10). Bounded to
    /// `PROC_CPU_SPARK_LEN` samples each; pruned to live pids every tick.
    pub proc_cpu_history: HashMap<u32, Ring<f32>>,
    /// Per-pid billed-power EWMA (macOS) — smooths the 2s sampler so
    /// the energy-hog insight doesn't fire on one busy window.
    pub proc_power_ewma: HashMap<u32, f32>,
    /// Per-pid leak tracking over detailed memory (footprint on macOS,
    /// private on Linux): pid → (baseline, ticks observed, latest).
    /// Only procs the memory sampler covers (top-N by RSS) are tracked.
    pub proc_mem_track: HashMap<u32, (u64, u32, u64)>,
    /// Full session: every snapshot pushed in order. Bounded — sized to
    /// match the metric rings so scrubbing stays in sync. The Timeline tab
    /// drives scrubbing; other tabs read App::displayed_snap().
    pub session: Ring<Snapshot>,
    /// Ring capacity, retained so per-device GPU series can be created lazily
    /// at the same depth as the fixed rings above.
    cap: usize,
}

impl History {
    pub(crate) fn new(cap: usize) -> Self {
        Self {
            cpu: Ring::new(cap),
            mem: Ring::new(cap),
            swap: Ring::new(cap),
            net_rate: Ring::new(cap),
            io_rate: Ring::new(cap),
            gpu_util: Ring::new(cap),
            gpu_util_by_name: HashMap::new(),
            gpu_vram_by_name: HashMap::new(),
            proc_cpu_ewma: HashMap::new(),
            proc_cpu_history: HashMap::new(),
            proc_power_ewma: HashMap::new(),
            proc_mem_track: HashMap::new(),
            session: Ring::new(cap),
            cap,
        }
    }

    pub(crate) fn push(&mut self, snap: &Snapshot) {
        // Mirror the snapshot into the session ring so scrubbing has full data.
        self.session.push(snap.clone());
        self.cpu.push(snap.cpu.usage_pct);
        let m = if snap.mem.total_bytes > 0 {
            (snap.mem.used_bytes as f32) / (snap.mem.total_bytes as f32)
        } else {
            0.0
        };
        self.mem.push(m);
        self.swap.push(snap.mem.swap_used_bytes);
        let net = snap.net.iter().map(|i| i.rx_rate + i.tx_rate).sum::<f64>();
        self.net_rate.push(net);
        self.io_rate
            .push(snap.disk_io.read_rate + snap.disk_io.write_rate);
        // Max util across all GPUs — handles laptops with iGPU+dGPU and the
        // common case of a single device alike. Defaults to 0 when no device
        // exposes util_pct.
        let gpu = snap
            .gpus
            .iter()
            .filter_map(|g| g.util_pct)
            .fold(0.0_f32, f32::max);
        self.gpu_util.push(gpu);

        // Per-device series for the GPU tab's per-card sparkline. Only record
        // a sample when the device actually reports util, so a device that
        // never exposes it stays absent from the map (honest "no data" rather
        // than a flat-zero line that looks like a genuinely idle GPU).
        let cap = self.cap;
        for g in &snap.gpus {
            if let Some(u) = g.util_pct {
                self.gpu_util_by_name
                    .entry(g.name.clone())
                    .or_insert_with(|| Ring::new(cap))
                    .push(u);
            }
            // VRAM used fraction, same only-when-reported rule as util.
            // Discrete GPUs report a dedicated VRAM total; Apple Silicon
            // (unified memory) does not, so fall back to total system RAM —
            // the real ceiling the GPU allocates against. Without this the
            // VRAM history stays empty on every Apple Silicon Mac even though
            // `vram_used_bytes` is reported every tick.
            if let Some(used) = g.vram_used_bytes {
                let total = g
                    .vram_total_bytes
                    .filter(|t| *t > 0)
                    .unwrap_or(snap.mem.total_bytes);
                if total > 0 {
                    self.gpu_vram_by_name
                        .entry(g.name.clone())
                        .or_insert_with(|| Ring::new(cap))
                        .push((used as f32 / total as f32).clamp(0.0, 1.0));
                }
            }
        }

        // Update per-pid EWMA. Alpha=0.3 → ~5 ticks to stabilize.
        // Prune pids that aren't in the current snapshot.
        let mut next: HashMap<u32, f32> = HashMap::with_capacity(snap.procs.len());
        for proc_ in &snap.procs {
            let prev = self
                .proc_cpu_ewma
                .get(&proc_.pid)
                .copied()
                .unwrap_or(proc_.cpu_pct);
            let ewma = 0.7 * prev + 0.3 * proc_.cpu_pct;
            next.insert(proc_.pid, ewma);
        }
        self.proc_cpu_ewma = next;

        // Per-pid CPU-usage history for the Procs sparkline (issue #10).
        // Move existing rings forward, append this tick's raw sample
        // (cpu_pct/100, clamped), and drop rings for pids that have exited.
        let mut next_hist: HashMap<u32, Ring<f32>> = HashMap::with_capacity(snap.procs.len());
        for proc_ in &snap.procs {
            let mut ring = self
                .proc_cpu_history
                .remove(&proc_.pid)
                .unwrap_or_else(|| Ring::new(PROC_CPU_SPARK_LEN));
            ring.push((proc_.cpu_pct / 100.0).clamp(0.0, 1.0));
            next_hist.insert(proc_.pid, ring);
        }
        self.proc_cpu_history = next_hist;

        // Billed-power EWMA, same alpha and same prune-to-live rule.
        let mut next_power: HashMap<u32, f32> = HashMap::new();
        for proc_ in &snap.procs {
            if let Some(w) = proc_.power_w {
                let prev = self.proc_power_ewma.get(&proc_.pid).copied().unwrap_or(w);
                next_power.insert(proc_.pid, 0.7 * prev + 0.3 * w);
            }
        }
        self.proc_power_ewma = next_power;

        // Leak tracking over the honest per-proc metric (footprint /
        // private). Baseline is the first detailed sighting; a proc
        // that drops out of the sampler's top-N keeps its entry until
        // it exits, so a slow leaker can't hide by being briefly idle.
        let live: std::collections::HashSet<u32> = snap.procs.iter().map(|p| p.pid).collect();
        self.proc_mem_track.retain(|pid, _| live.contains(pid));
        for proc_ in &snap.procs {
            let Some(metric) = proc_.mem_footprint.or(proc_.mem_private) else {
                continue;
            };
            self.proc_mem_track
                .entry(proc_.pid)
                .and_modify(|(_base, ticks, latest)| {
                    *ticks += 1;
                    *latest = metric;
                })
                .or_insert((metric, 1, metric));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabId {
    Overview,
    Cpu,
    Memory,
    Disks,
    Fs,
    Procs,
    Gpu,
    Power,
    Services,
    Net,
    Timeline,
    Insights,
}

pub const ALL_TABS: &[TabId] = &[
    TabId::Overview,
    TabId::Cpu,
    TabId::Memory,
    TabId::Disks,
    TabId::Fs,
    TabId::Procs,
    TabId::Gpu,
    TabId::Power,
    TabId::Services,
    TabId::Net,
    TabId::Timeline,
    TabId::Insights,
];

impl TabId {
    pub fn glyph(&self) -> &'static str {
        match self {
            TabId::Overview => "1",
            TabId::Cpu => "2",
            TabId::Memory => "3",
            TabId::Disks => "4",
            TabId::Fs => "5",
            TabId::Procs => "6",
            TabId::Gpu => "7",
            TabId::Power => "8",
            TabId::Services => "9",
            TabId::Net => "0",
            TabId::Timeline => "-",
            TabId::Insights => "+",
        }
    }
    /// Tabs whose main table can be narrowed with the `/` (or `f`) filter.
    /// Everything else is a fixed-shape dashboard with nothing to search.
    /// The footer and help popup key off this, so a tab that gains a
    /// filterable list only has to be added here (issue #20).
    pub fn supports_filter(&self) -> bool {
        matches!(self, TabId::Procs | TabId::Memory | TabId::Services)
    }
    pub fn title(&self) -> &'static str {
        match self {
            TabId::Overview => "Overview",
            TabId::Cpu => "CPU",
            TabId::Memory => "Memory",
            TabId::Disks => "Disks",
            TabId::Fs => "FS",
            TabId::Procs => "Procs",
            TabId::Gpu => "GPU",
            TabId::Power => "Power",
            TabId::Services => "Services",
            TabId::Net => "Net",
            TabId::Timeline => "Timeline",
            TabId::Insights => "Insights",
        }
    }
    fn from_str_loose(s: &str) -> Option<TabId> {
        match s.to_ascii_lowercase().as_str() {
            "overview" | "1" => Some(TabId::Overview),
            "cpu" | "2" => Some(TabId::Cpu),
            "memory" | "mem" | "3" => Some(TabId::Memory),
            "disks" | "disk" | "4" => Some(TabId::Disks),
            "fs" | "filesystems" | "5" => Some(TabId::Fs),
            "procs" | "processes" | "6" => Some(TabId::Procs),
            "gpu" | "7" => Some(TabId::Gpu),
            "power" | "8" => Some(TabId::Power),
            "services" | "9" => Some(TabId::Services),
            "net" | "network" | "0" => Some(TabId::Net),
            "timeline" | "-" => Some(TabId::Timeline),
            "insights" | "+" => Some(TabId::Insights),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcSort {
    Cpu,
    /// Labelled "mem" — orders by mem_rss, which is the same order as
    /// the %MEM column (total RAM is constant across rows).
    Rss,
    Io,
    Start,
    Name,
    /// %GPU desc — None values sort to the bottom so platforms
    /// without per-PID GPU data don't put empty rows at the top.
    Gpu,
    /// Combined net rx+tx desc — same None-to-bottom rule.
    Net,
}

impl ProcSort {
    pub fn label(&self) -> &'static str {
        match self {
            ProcSort::Cpu => "cpu",
            ProcSort::Rss => "mem",
            ProcSort::Io => "io",
            ProcSort::Start => "start",
            ProcSort::Name => "name",
            ProcSort::Gpu => "gpu",
            ProcSort::Net => "net",
        }
    }
    pub const ALL: [ProcSort; 7] = [
        ProcSort::Cpu,
        ProcSort::Rss,
        ProcSort::Io,
        ProcSort::Start,
        ProcSort::Name,
        ProcSort::Gpu,
        ProcSort::Net,
    ];
    fn next(self) -> ProcSort {
        let i = ProcSort::ALL.iter().position(|s| *s == self).unwrap_or(0);
        ProcSort::ALL[(i + 1) % ProcSort::ALL.len()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceSort {
    Name,
    Status,
    Pid,
}

impl ServiceSort {
    pub const ALL: [ServiceSort; 3] = [ServiceSort::Name, ServiceSort::Status, ServiceSort::Pid];
    pub fn label(&self) -> &'static str {
        match self {
            ServiceSort::Name => "name",
            ServiceSort::Status => "status",
            ServiceSort::Pid => "pid",
        }
    }
    fn next(self) -> ServiceSort {
        let i = ServiceSort::ALL
            .iter()
            .position(|s| *s == self)
            .unwrap_or(0);
        ServiceSort::ALL[(i + 1) % ServiceSort::ALL.len()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveState {
    Live,
    Paused,
    Scrub,
    /// Replay mode — `--replay path.swr` was passed. No live data,
    /// the scrubber walks recorded snapshots.
    Replay,
}

/// Which of the two top-level views is rendering.
///
/// Lite is a deliberate counterpart to the full tabbed TUI: one screen at
/// 80×24 answering "why is this machine hot, slow, or loud?" It is
/// **opt-in only** — entered with `--lite` or toggled with `L`, never
/// auto-selected by terminal size. Both views share the same collector and
/// the same tick loop, so toggling is instant and no history is lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    #[default]
    Full,
    Lite,
}

/// Lite's own view state. Deliberately separate from the full TUI's
/// `proc_sel` / `filter_*`: the two views have different row sets and
/// different filter semantics (Lite matches name *or* user), and sharing
/// them would make toggling views move the other view's cursor.
#[derive(Debug, Clone, Default)]
pub struct LiteState {
    pub selected: usize,
    pub offset: usize,
    pub detail_open: bool,
    pub filter_input: bool,
    pub filter_text: String,
    /// Threshold debounce for the alert state, advanced once per sample.
    pub alerts: crate::ui::lite::AlertTracker,
}

pub struct App {
    pub active: TabId,
    pub paused: bool,
    /// Full tabbed TUI or the single-screen Lite view.
    pub view_mode: ViewMode,
    /// Lite's selection, filter and alert state.
    pub lite: LiteState,
    /// Terminal area from the last frame. Lite's key handler needs to know
    /// how many rows are visible in order to clamp scrolling, and only the
    /// renderer sees the real `Rect`.
    pub last_area: Rect,
    pub history: History,
    pub snap: Option<Snapshot>,
    pub proc_sort: ProcSort,
    pub proc_sel: usize,
    /// Whether the Procs tab shows its drill-in detail pane (which carries the
    /// selected process's started date and other per-proc detail). Toggled
    /// with `d`; off reclaims those rows for the process list (issue #13).
    pub proc_show_detail: bool,
    pub service_sort: ServiceSort,
    pub service_sel: usize,
    pub insights: Vec<Insight>,
    /// Scrub offset in ticks back from "now" (0 = live). Driven by Timeline's
    /// arrow keys; clamped to session length. Affects every tab via
    /// App::displayed_snap.
    pub scrub_offset: usize,
    /// Chart rendering style. Toggled with `g`. Affects every multi-row
    /// sparkline tile (CPU/Net/Disks aggregates, Overview KPIs).
    pub graph_style: GraphStyle,

    // ── Settings popup state ─────────────────────────────────────────────
    /// In-memory user config. Mutated by the settings popup; written to
    /// disk only when the user presses S. `t`/`g` mutate runtime state
    /// AND mirror into this struct so that opening settings reflects
    /// current values.
    pub user_config: SyswatchConfig,
    /// Whether the settings popup is open. Routes key input through
    /// `crate::ui::settings` while true.
    pub settings_active: bool,
    /// Cursor row inside the settings popup.
    pub settings_cursor: usize,
    /// Whether the popup is in text-edit mode for the cursor row
    /// (numerics — currently just `tick_ms`).
    pub settings_editing: bool,
    /// Buffer for in-progress text edits.
    pub settings_edit_buf: String,
    /// Status line shown under the popup body — "saved", validation
    /// errors, etc.
    pub settings_status: Option<String>,

    /// Transient status flash shown in the footer — used by the `S`
    /// snapshot command to confirm the dump path. `(message, expires_at)`.
    pub footer_flash: Option<(String, Instant)>,

    /// Whether the `?` help popup is open. Absorbs all input while true
    /// (Esc / `?` to close).
    pub help_active: bool,

    // ── Table filter (`/` or `f` key) ───────────────────────────────────
    /// True while the user is typing into the filter input box. Esc
    /// cancels (drops both the buffer and any active filter); Enter
    /// commits the buffer to `filter_active`.
    pub filter_input: bool,
    /// In-progress filter text. Applied live to the visible table while
    /// typing so the user sees match results immediately.
    pub filter_buf: String,
    /// Currently-applied filter (case-insensitive substring match).
    /// None means no filter.
    ///
    /// One filter shared by every `TabId::supports_filter` tab rather
    /// than one per tab: the question a user is asking ("where is
    /// chrome?") doesn't change when they move from Procs to Memory,
    /// so carrying it across is the useful behaviour (issue #20).
    pub filter_active: Option<String>,

    /// Active session recorder, when the user has pressed `R`. None
    /// means not recording. Drop on quit flushes the buffered tail.
    pub recorder: Option<crate::recording::Recorder>,

    /// True when the app was launched with --replay; suppresses live
    /// collection and changes the LiveState badge.
    pub replay_mode: bool,
}

impl App {
    /// Render-time graph options bundle. Built once per render path from
    /// `user_config.graph_fade`; passed to every `graph::render` call site
    /// so a single config toggle drives the entire UI.
    pub fn graph_opts(&self) -> crate::ui::graph::GraphOpts {
        crate::ui::graph::GraphOpts {
            fade: self.user_config.graph_fade,
        }
    }

    pub fn displayed_snap(&self) -> Option<&Snapshot> {
        if self.scrub_offset > 0 {
            self.history.session.nth_back(self.scrub_offset)
        } else {
            self.snap.as_ref()
        }
    }
    pub fn live_state(&self) -> LiveState {
        if self.replay_mode {
            return LiveState::Replay;
        }
        if self.scrub_offset > 0 {
            LiveState::Scrub
        } else if self.paused {
            LiveState::Paused
        } else {
            LiveState::Live
        }
    }
}

impl App {
    fn new(start: TabId, config: SyswatchConfig) -> Self {
        // Theme is already applied globally in main(). Resolve graph_style
        // from the same config.
        let graph_style = match config.graph_style.to_lowercase().as_str() {
            "dots" => GraphStyle::Dots,
            _ => GraphStyle::Bars,
        };
        Self {
            active: start,
            paused: false,
            view_mode: ViewMode::Full,
            lite: LiteState::default(),
            last_area: Rect::new(0, 0, 0, 0),
            history: History::new(120),
            snap: None,
            proc_sort: ProcSort::Cpu,
            proc_sel: 0,
            proc_show_detail: true,
            service_sort: ServiceSort::Name,
            service_sel: 0,
            insights: Vec::new(),
            scrub_offset: 0,
            graph_style,
            user_config: config,
            settings_active: false,
            settings_cursor: 0,
            settings_editing: false,
            settings_edit_buf: String::new(),
            settings_status: None,
            footer_flash: None,
            help_active: false,
            filter_input: false,
            filter_buf: String::new(),
            filter_active: None,
            recorder: None,
            replay_mode: false,
        }
    }

    /// Lite's key handler. Six advertised keys plus the two conventions the
    /// footer doesn't have room to advertise (`↑↓`/`jk` and `Esc`).
    fn handle_lite_key(&mut self, k: KeyEvent) -> bool {
        use crate::ui::lite;

        // Filter input swallows printable keys, so it has to be checked
        // before anything that reads a bare char.
        if self.lite.filter_input {
            match (k.code, k.modifiers) {
                (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
                (KeyCode::Esc, _) => {
                    self.lite.filter_input = false;
                    self.lite.filter_text.clear();
                    self.lite.selected = 0;
                    self.lite.offset = 0;
                }
                // Commit but keep it applied — the list stays narrowed so
                // ↵ can then open detail on a match.
                (KeyCode::Enter, _) => self.lite.filter_input = false,
                (KeyCode::Backspace, _) => {
                    self.lite.filter_text.pop();
                    self.lite.selected = 0;
                    self.lite.offset = 0;
                }
                (KeyCode::Char(c), _) => {
                    self.lite.filter_text.push(c);
                    self.lite.selected = 0;
                    self.lite.offset = 0;
                }
                _ => {}
            }
            return false;
        }

        let count = match self.displayed_snap() {
            Some(snap) => lite::filter_procs(
                lite::collect_procs(snap, &self.history),
                &self.lite.filter_text,
            )
            .len(),
            None => 0,
        };
        let visible = lite::Layout::new(self.last_area).visible_procs(self.lite.detail_open);

        match (k.code, k.modifiers) {
            (KeyCode::Char('q'), _) => return true,
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
            (KeyCode::Char('L'), _) => {
                // The other half of the toggle; the full TUI owns Full→Lite.
                self.view_mode = ViewMode::Full;
                self.lite.detail_open = false;
                self.lite.filter_input = false;
            }
            (KeyCode::Char('?'), _) => self.help_active = true,
            (KeyCode::Char('p'), _) => self.paused = !self.paused,
            (KeyCode::Char('/'), _) => {
                self.lite.filter_input = true;
                self.lite.filter_text.clear();
            }
            // Expands whatever is selected. The selection never moves.
            (KeyCode::Enter, _) => self.lite.detail_open = !self.lite.detail_open,
            (KeyCode::Esc, _) => {
                // Esc unwinds one layer at a time: detail, then the filter.
                if self.lite.detail_open {
                    self.lite.detail_open = false;
                } else if !self.lite.filter_text.is_empty() {
                    self.lite.filter_text.clear();
                    self.lite.selected = 0;
                    self.lite.offset = 0;
                }
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => {
                if count > 0 {
                    self.lite.selected = (self.lite.selected + 1).min(count - 1);
                }
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => {
                self.lite.selected = self.lite.selected.saturating_sub(1);
            }
            _ => {}
        }

        clamp_lite_scroll(&mut self.lite, count, visible);
        false
    }

    /// Send every filterable tab's cursor back to the top. The filter is
    /// shared across tabs, so changing it invalidates each of their
    /// selections — not just the one currently on screen.
    fn reset_filtered_selection(&mut self) {
        self.proc_sel = 0;
        self.service_sel = 0;
    }

    fn handle_key(&mut self, k: KeyEvent) -> bool {
        if k.kind != KeyEventKind::Press {
            return false;
        }
        // Lite owns its whole key surface — it has six advertised keys, not
        // twelve tabs — so it routes before any of the full TUI's handling.
        // The `?` overlay is the one thing the two views share.
        if self.view_mode == ViewMode::Lite && !self.help_active {
            return self.handle_lite_key(k);
        }
        // Filter input mode — narrow keyboard scope so chars typed into
        // the search box don't also fire dashboard hotkeys.
        if self.filter_input {
            match (k.code, k.modifiers) {
                (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
                (KeyCode::Esc, _) => {
                    // Cancel: drop both the in-progress text and any
                    // currently-applied filter.
                    self.filter_input = false;
                    self.filter_buf.clear();
                    self.filter_active = None;
                    self.reset_filtered_selection();
                }
                (KeyCode::Enter, _) => {
                    self.filter_input = false;
                    self.filter_active = if self.filter_buf.is_empty() {
                        None
                    } else {
                        Some(self.filter_buf.clone())
                    };
                    self.reset_filtered_selection();
                }
                (KeyCode::Backspace, _) => {
                    self.filter_buf.pop();
                    // Live-apply so the table updates as the user types.
                    self.filter_active = if self.filter_buf.is_empty() {
                        None
                    } else {
                        Some(self.filter_buf.clone())
                    };
                    self.reset_filtered_selection();
                }
                (KeyCode::Char(c), _) => {
                    self.filter_buf.push(c);
                    self.filter_active = Some(self.filter_buf.clone());
                    self.reset_filtered_selection();
                }
                _ => {}
            }
            return false;
        }
        // Help popup is the simplest modal — Esc, `?`, or Ctrl-C close
        // it; every other key is swallowed so the user can't accidentally
        // act on the dashboard behind the popup.
        if self.help_active {
            match (k.code, k.modifiers) {
                (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
                (KeyCode::Esc, _) | (KeyCode::Char('?'), _) => self.help_active = false,
                _ => {}
            }
            return false;
        }
        // Settings popup absorbs all input while active. Returns true if
        // the popup wants the parent app to ignore the key entirely.
        if self.settings_active {
            return self.handle_settings_key(k);
        }
        match (k.code, k.modifiers) {
            (KeyCode::Char('q'), _) => return true,
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
            (KeyCode::Char('p'), _) => self.paused = !self.paused,
            (KeyCode::Char(','), _) => {
                self.settings_active = true;
                self.settings_cursor = 0;
                self.settings_status = None;
                self.settings_editing = false;
                return false;
            }
            (KeyCode::Char('?'), _) => {
                self.help_active = true;
                return false;
            }
            (KeyCode::Char('S'), _) => {
                // Dump the currently-displayed snapshot (live or scrubbed).
                // Status flash shows the resulting path for ~3s.
                let msg = match self.displayed_snap() {
                    Some(snap) => match crate::snapshot::write(snap) {
                        Ok(path) => format!("snapshot → {}", path.display()),
                        Err(e) => format!("snapshot failed: {}", e),
                    },
                    None => "no snapshot yet — wait for first sample".into(),
                };
                self.footer_flash = Some((msg, Instant::now() + Duration::from_secs(3)));
            }
            (KeyCode::Char('R'), _) => {
                // Toggle session recording. Each tick after start gets
                // appended; pressing R again (or quitting) flushes and
                // closes the file.
                let msg = if let Some(rec) = self.recorder.take() {
                    let path = rec.path().display().to_string();
                    let count = rec.count;
                    drop(rec); // explicit flush via Drop
                    format!("recording stopped → {} ({} ticks)", path, count)
                } else {
                    match crate::recording::fresh_path() {
                        Some(p) => match crate::recording::Recorder::create(p) {
                            Ok(rec) => {
                                let path = rec.path().display().to_string();
                                self.recorder = Some(rec);
                                format!("recording → {}", path)
                            }
                            Err(e) => format!("recording failed: {}", e),
                        },
                        None => "cannot determine local data dir".into(),
                    }
                };
                self.footer_flash = Some((msg, Instant::now() + Duration::from_secs(3)));
            }
            (KeyCode::Char('g'), _) => {
                self.graph_style = self.graph_style.next();
                // Mirror into user_config so the settings popup sees the
                // current value. Disk write happens only on S in settings.
                self.user_config.graph_style = self.graph_style.label().into();
            }
            (KeyCode::Char('t'), _) => {
                let next = crate::ui::theme::cycle();
                self.user_config.theme = next.into();
            }
            // The other half of the Lite toggle; `handle_lite_key` owns Lite→Full.
            (KeyCode::Char('L'), _) => self.view_mode = ViewMode::Lite,
            (KeyCode::Char('1'), _) => self.active = TabId::Overview,
            (KeyCode::Char('2'), _) => self.active = TabId::Cpu,
            (KeyCode::Char('3'), _) => self.active = TabId::Memory,
            (KeyCode::Char('4'), _) => self.active = TabId::Disks,
            (KeyCode::Char('5'), _) => self.active = TabId::Fs,
            (KeyCode::Char('6'), _) => self.active = TabId::Procs,
            (KeyCode::Char('7'), _) => self.active = TabId::Gpu,
            (KeyCode::Char('8'), _) => self.active = TabId::Power,
            (KeyCode::Char('9'), _) => self.active = TabId::Services,
            (KeyCode::Char('0'), _) => self.active = TabId::Net,
            (KeyCode::Char('-'), _) => self.active = TabId::Timeline,
            (KeyCode::Char('+') | KeyCode::Char('='), _) => self.active = TabId::Insights,
            (KeyCode::Tab, _) => self.active = next_tab(self.active),
            (KeyCode::BackTab, _) => self.active = prev_tab(self.active),
            (KeyCode::Up, _) if self.active == TabId::Procs => {
                self.proc_sel = self.proc_sel.saturating_sub(1);
            }
            (KeyCode::Down, _) if self.active == TabId::Procs => {
                // Clamp against the *filtered* list — netwatch issue #26
                // taught us not to let selection land on rows the user
                // can't see.
                let max = self
                    .snap
                    .as_ref()
                    .map(|s| {
                        crate::tabs::procs::filtered_sorted(
                            &s.procs,
                            self.proc_sort,
                            self.filter_active.as_deref(),
                        )
                        .len()
                        .saturating_sub(1)
                    })
                    .unwrap_or(0);
                self.proc_sel = (self.proc_sel + 1).min(max);
            }
            (KeyCode::Char('s'), _) if self.active == TabId::Procs => {
                self.proc_sort = self.proc_sort.next();
                self.proc_sel = 0;
            }
            (KeyCode::Char('/') | KeyCode::Char('f'), _) if self.active.supports_filter() => {
                // Enter filter input mode. Pre-fill with the current
                // applied filter (if any) so the user can refine it.
                //
                // `f` is an alternate trigger for `/`: on some keyboard
                // layouts (e.g. ergol, issue #18) `/` lives on the AltGr
                // layer and doesn't reliably reach us as a plain Char('/'),
                // leaving search unreachable. `f` is a base-layer letter on
                // every layout and matches btop/glances' filter key.
                self.filter_input = true;
                self.filter_buf = self.filter_active.clone().unwrap_or_default();
            }
            (KeyCode::Char('d'), _) if self.active == TabId::Procs => {
                // Collapse/expand the drill-in detail pane to trade detail
                // (incl. the started date) for more process rows (issue #13).
                self.proc_show_detail = !self.proc_show_detail;
            }
            (KeyCode::Up, _) if self.active == TabId::Services => {
                self.service_sel = self.service_sel.saturating_sub(1);
            }
            (KeyCode::Down, _) if self.active == TabId::Services => {
                // Clamp against the *filtered* list, same as Procs — a
                // selection past the last visible row is invisible and
                // scrolls the detail pane onto a service the user can't see.
                let max = self
                    .snap
                    .as_ref()
                    .map(|s| {
                        crate::tabs::services::filtered_sorted(
                            &s.services,
                            self.service_sort,
                            self.filter_active.as_deref(),
                        )
                        .len()
                        .saturating_sub(1)
                    })
                    .unwrap_or(0);
                self.service_sel = (self.service_sel + 1).min(max);
            }
            (KeyCode::Char('s'), _) if self.active == TabId::Services => {
                self.service_sort = self.service_sort.next();
                self.service_sel = 0;
            }
            // Scrub controls: active on every tab, but most useful on Timeline.
            (KeyCode::Left, _) => {
                let max = self.history.session.len().saturating_sub(1);
                self.scrub_offset = (self.scrub_offset + 1).min(max);
            }
            (KeyCode::Right, _) => {
                self.scrub_offset = self.scrub_offset.saturating_sub(1);
            }
            (KeyCode::Home, _) => {
                self.scrub_offset = self.history.session.len().saturating_sub(1);
            }
            (KeyCode::End, _) => {
                self.scrub_offset = 0;
            }
            _ => {}
        }
        false
    }
}

impl App {
    /// Settings-popup key router. Returns true to quit the app (only on
    /// Ctrl-C); false otherwise.
    fn handle_settings_key(&mut self, k: KeyEvent) -> bool {
        use crate::ui::settings;
        if self.settings_editing {
            match k.code {
                KeyCode::Esc => {
                    self.settings_editing = false;
                    self.settings_edit_buf.clear();
                    self.settings_status = None;
                }
                KeyCode::Enter => {
                    let buf = std::mem::take(&mut self.settings_edit_buf);
                    match settings::apply_edit(&mut self.user_config, self.settings_cursor, &buf) {
                        Ok(()) => {
                            self.settings_editing = false;
                            self.settings_status = Some("applied (press S to save to disk)".into());
                        }
                        Err(msg) => {
                            self.settings_status = Some(msg);
                            // Keep editing so user can retry.
                            self.settings_edit_buf = buf;
                        }
                    }
                }
                KeyCode::Backspace => {
                    self.settings_edit_buf.pop();
                }
                KeyCode::Char(c) => {
                    self.settings_edit_buf.push(c);
                }
                _ => {}
            }
            return false;
        }
        match (k.code, k.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
            (KeyCode::Esc, _) => {
                self.settings_active = false;
                self.settings_status = None;
            }
            (KeyCode::Up, _) => {
                self.settings_cursor = self.settings_cursor.saturating_sub(1);
                self.settings_status = None;
            }
            (KeyCode::Down, _) => {
                self.settings_cursor = (self.settings_cursor + 1).min(settings::ROWS - 1);
                self.settings_status = None;
            }
            (KeyCode::Left, _) => {
                settings::cycle_prev(&mut self.user_config, self.settings_cursor);
                self.apply_runtime_from_config();
            }
            (KeyCode::Right, _) => {
                settings::cycle_next(&mut self.user_config, self.settings_cursor);
                self.apply_runtime_from_config();
            }
            (KeyCode::Enter, _) => {
                // Enter is only meaningful for non-enum (text) rows.
                self.settings_edit_buf =
                    settings::edit_value(&self.user_config, self.settings_cursor);
                self.settings_editing = true;
                self.settings_status = None;
            }
            (KeyCode::Char('s' | 'S'), _) => match self.user_config.save() {
                Ok(()) => {
                    self.settings_status = Some(format!(
                        "saved to {}",
                        crate::config::SyswatchConfig::path()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "config dir".into())
                    ));
                }
                Err(e) => self.settings_status = Some(format!("save failed: {}", e)),
            },
            _ => {}
        }
        false
    }

    /// Sync runtime state (theme + graph_style) from `user_config`. Called
    /// after each ←/→ cycle in the settings popup so the user sees the
    /// effect immediately on the dashboard behind the popup.
    fn apply_runtime_from_config(&mut self) {
        crate::ui::theme::set_by_name(&self.user_config.theme);
        self.graph_style = match self.user_config.graph_style.to_lowercase().as_str() {
            "dots" => GraphStyle::Dots,
            _ => GraphStyle::Bars,
        };
        // `graph_fade` deliberately has no cached runtime copy on `App` —
        // `App::graph_opts()` reads it from `user_config` live each render
        // tick, so the toggle takes effect on the very next frame without
        // a sync step here. Don't add a redundant mirror; it'll drift.
    }
}

fn next_tab(t: TabId) -> TabId {
    let i = ALL_TABS.iter().position(|x| *x == t).unwrap_or(0);
    ALL_TABS[(i + 1) % ALL_TABS.len()]
}

fn prev_tab(t: TabId) -> TabId {
    let i = ALL_TABS.iter().position(|x| *x == t).unwrap_or(0);
    ALL_TABS[(i + ALL_TABS.len() - 1) % ALL_TABS.len()]
}

/// Keep the selection inside the visible window, and — when detail is open —
/// far enough from the bottom that its three rows fit before the prompt row.
///
/// This is what lets `↵` expand in place without moving the selection: if the
/// selected row would fall outside the shortened list, the list scrolls to it
/// rather than the cursor jumping to the list.
fn clamp_lite_scroll(lite: &mut LiteState, count: usize, visible: u16) {
    if count == 0 {
        lite.selected = 0;
        lite.offset = 0;
        return;
    }
    lite.selected = lite.selected.min(count - 1);
    let visible = visible.max(1) as usize;

    if lite.selected < lite.offset {
        lite.offset = lite.selected;
    }
    // The detail block consumes rows *below* the selected row, so when it's
    // open the selection must sit at least DETAIL_ROWS above the window
    // bottom. `visible` already excludes those rows, so the same arithmetic
    // covers both cases.
    if lite.selected >= lite.offset + visible {
        lite.offset = lite.selected + 1 - visible;
    }
    let max_offset = count.saturating_sub(visible);
    lite.offset = lite.offset.min(max_offset);
}

pub fn run(opts: Options) -> Result<()> {
    // Replay mode forces Timeline as the starting tab — that's where
    // the scrubber lives — unless the user explicitly passed --tab.
    let user_picked_tab = opts.start_tab.is_some();
    let start = opts
        .start_tab
        .as_deref()
        .and_then(TabId::from_str_loose)
        .unwrap_or(TabId::Overview);
    let start = if opts.replay.is_some() && !user_picked_tab {
        TabId::Timeline
    } else {
        start
    };

    let mut app = App::new(start, opts.config);
    if opts.lite {
        app.view_mode = ViewMode::Lite;
    }

    // Populate History from the recording up front, then plant the
    // scrubber at oldest tick so the user sees the start. Live
    // collection is skipped entirely in replay mode.
    if let Some(snaps) = opts.replay {
        app.replay_mode = true;
        // Resize the History rings if needed so the entire recording
        // fits — default cap is 120 ticks; sessions longer than that
        // would otherwise lose the head on push.
        let needed = snaps.len().max(120);
        app.history = History::new(needed);
        for s in &snaps {
            app.history.push(s);
        }
        // Park scrubber at the oldest tick so the user explores
        // forward through the recording.
        app.scrub_offset = app.history.session.len().saturating_sub(1);
        app.snap = snaps.last().cloned();
        app.insights = if let Some(last) = snaps.last() {
            insights::compute(&app.history, last)
        } else {
            Vec::new()
        };
    }

    // Skip collector setup in replay mode — IOReport / system_profiler
    // / ioreg probes do real work we don't need when there's no live
    // sampling to do.
    let mut collector: Option<Collector> = if app.replay_mode {
        None
    } else {
        Some(Collector::new(app.user_config.tick_ms))
    };

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut term = Terminal::new(backend)?;

    // Force the first sample to fire immediately. After that we re-read
    // the tick interval from `user_config` on every iteration so changes
    // made through the settings popup take effect on the next cycle
    // without a restart.
    let mut last_tick = Instant::now() - Duration::from_secs(60);
    let res = loop {
        // 100..=5000 ms — matches the validation in `config::validate`
        // and `settings::apply_edit`. The clamp is defensive in case a
        // hand-edited config slipped through.
        let tick = Duration::from_millis(app.user_config.tick_ms.clamp(100, 5000));
        if last_tick.elapsed() >= tick {
            if !app.paused {
                if let Some(c) = collector.as_mut() {
                    let s = c.sample();
                    app.history.push(&s);
                    app.insights = insights::compute(&app.history, &s);
                    // Advance Lite's threshold debounce once per *sample*.
                    // Doing this at render time would tie the alert window
                    // to the frame rate instead of the collection interval.
                    let swap_rate =
                        crate::ui::lite::swap_rate(&app.history, app.user_config.tick_ms);
                    app.lite.alerts.update(&s, swap_rate);
                    // Append to active recording (best-effort — we
                    // don't want one bad write to brick the live UI).
                    if let Some(rec) = app.recorder.as_mut() {
                        if let Err(e) = rec.push(&s) {
                            app.footer_flash = Some((
                                format!("recording: {}", e),
                                Instant::now() + Duration::from_secs(3),
                            ));
                            app.recorder = None;
                        }
                    }
                    app.snap = Some(s);
                }
                // Replay mode: nothing to sample, the History ring is
                // pre-populated and the scrubber drives displayed_snap.
            }
            last_tick = Instant::now();
        }

        // Lite's key handler needs the frame geometry to clamp scrolling,
        // and only the renderer normally sees it.
        if let Ok(size) = term.size() {
            app.last_area = Rect::new(0, 0, size.width, size.height);
        }

        if let Some(snap) = app.displayed_snap() {
            term.draw(|f| draw(f, &app, snap))?;
        }

        let timeout = tick.saturating_sub(last_tick.elapsed());
        if event::poll(timeout.max(Duration::from_millis(33)))? {
            match event::read()? {
                Event::Key(k) => {
                    if app.handle_key(k) {
                        break Ok::<(), anyhow::Error>(());
                    }
                }
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
    };

    disable_raw_mode()?;
    execute!(term.backend_mut(), LeaveAlternateScreen)?;
    res?;
    Ok(())
}

fn draw(f: &mut ratatui::Frame, app: &App, snap: &Snapshot) {
    let area = f.area();
    if area.width < 20 || area.height < 6 {
        return;
    }
    // Lite owns the whole frame — no header, no tab bar, no footer chrome.
    // That is the point: one screen, and every row of it is content.
    if app.view_mode == ViewMode::Lite {
        crate::ui::lite::render(f, app, snap);
        if app.help_active {
            crate::ui::help::render(f, area);
        }
        return;
    }
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Length(2), // tab bar (label + underline)
            Constraint::Min(0),    // body
            Constraint::Length(2), // footer (separator + hotkeys)
        ])
        .split(area);

    chrome::draw_header(
        f,
        chunks[0],
        snap,
        app.live_state(),
        app.recorder.is_some(),
        app.user_config.tick_ms,
    );
    let active_insights = app
        .insights
        .iter()
        .filter(|i| i.severity != insights::Severity::Info)
        .count();
    chrome::draw_tab_bar(f, chunks[1], app.active, active_insights);
    let body = Rect {
        x: chunks[2].x,
        y: chunks[2].y,
        width: chunks[2].width,
        height: chunks[2].height,
    };
    tabs::draw(f, body, app, snap);
    // Drain expired footer flashes before drawing.
    let flash = app
        .footer_flash
        .as_ref()
        .filter(|(_, expires)| Instant::now() < *expires)
        .map(|(msg, _)| msg.as_str());
    chrome::draw_footer(
        f,
        chunks[3],
        app.graph_style,
        flash,
        app.active.supports_filter(),
    );

    // Modal popups paint over the dashboard. Help wins ties since it's
    // a hard requirement to read the docs even with settings open
    // (in practice both modals can't be open at once, but this is the
    // safe ordering).
    if app.settings_active {
        crate::ui::settings::render(f, app, area);
    }
    if app.help_active {
        crate::ui::help::render(f, area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::ProcTick;

    fn proc(pid: u32, cpu: f32) -> ProcTick {
        ProcTick {
            pid,
            cpu_pct: cpu,
            ..Default::default()
        }
    }

    fn snap_with(procs: Vec<ProcTick>) -> Snapshot {
        Snapshot {
            procs,
            ..Default::default()
        }
    }

    #[test]
    fn ewma_first_observation_is_value_itself() {
        let mut h = History::new(10);
        h.push(&snap_with(vec![proc(42, 80.0)]));
        // No prior reading → ewma = 0.7 * value + 0.3 * value = value.
        assert_eq!(h.proc_cpu_ewma.get(&42).copied(), Some(80.0));
    }

    #[test]
    fn ewma_converges_to_steady_state() {
        let mut h = History::new(20);
        // Stable signal at 100% over many ticks should pull EWMA toward 100.
        for _ in 0..15 {
            h.push(&snap_with(vec![proc(1, 100.0)]));
        }
        let v = h.proc_cpu_ewma.get(&1).copied().unwrap();
        assert!((v - 100.0).abs() < 0.01, "expected ≈100, got {}", v);
    }

    #[test]
    fn ewma_smooths_a_spike() {
        let mut h = History::new(20);
        for _ in 0..5 {
            h.push(&snap_with(vec![proc(1, 0.0)]));
        }
        // One transient spike to 100%.
        h.push(&snap_with(vec![proc(1, 100.0)]));
        let v = h.proc_cpu_ewma.get(&1).copied().unwrap();
        // Should be much less than 100 — the spike doesn't dominate.
        assert!(v > 20.0 && v < 50.0, "expected ~30, got {}", v);
    }

    #[test]
    fn ewma_prunes_pids_absent_from_latest_snapshot() {
        let mut h = History::new(10);
        h.push(&snap_with(vec![proc(1, 50.0), proc(2, 50.0)]));
        assert!(h.proc_cpu_ewma.contains_key(&1));
        assert!(h.proc_cpu_ewma.contains_key(&2));

        // pid 2 disappears.
        h.push(&snap_with(vec![proc(1, 50.0)]));
        assert!(h.proc_cpu_ewma.contains_key(&1));
        assert!(!h.proc_cpu_ewma.contains_key(&2));
    }

    // ── per-pid CPU history sparkline (issue #10) ───────────────────────

    #[test]
    fn cpu_history_records_normalized_samples() {
        let mut h = History::new(10);
        h.push(&snap_with(vec![proc(1, 50.0)]));
        h.push(&snap_with(vec![proc(1, 100.0)]));
        let ring = h.proc_cpu_history.get(&1).expect("history for pid 1");
        // cpu_pct/100, oldest→newest.
        assert_eq!(ring.to_vec(), vec![0.5, 1.0]);
    }

    #[test]
    fn cpu_history_clamps_above_100pct() {
        // Multi-core aggregate cpu_pct can exceed 100; sparkline samples
        // are clamped into 0..1 so the glyph mapping stays in range.
        let mut h = History::new(10);
        h.push(&snap_with(vec![proc(1, 350.0)]));
        assert_eq!(h.proc_cpu_history.get(&1).unwrap().to_vec(), vec![1.0]);
    }

    #[test]
    fn cpu_history_is_bounded_to_spark_len() {
        let mut h = History::new(10);
        for _ in 0..(PROC_CPU_SPARK_LEN + 5) {
            h.push(&snap_with(vec![proc(1, 10.0)]));
        }
        assert_eq!(
            h.proc_cpu_history.get(&1).unwrap().len(),
            PROC_CPU_SPARK_LEN
        );
    }

    #[test]
    fn cpu_history_prunes_dead_pids() {
        let mut h = History::new(10);
        h.push(&snap_with(vec![proc(1, 10.0), proc(2, 10.0)]));
        h.push(&snap_with(vec![proc(1, 10.0)]));
        assert!(h.proc_cpu_history.contains_key(&1));
        assert!(!h.proc_cpu_history.contains_key(&2));
    }

    #[test]
    fn gpu_util_by_name_records_only_devices_reporting_util() {
        use crate::collect::GpuTick;
        let mut h = History::new(10);
        let snap = Snapshot {
            gpus: vec![
                GpuTick {
                    name: "Apple M3 Max".into(),
                    util_pct: Some(42.0),
                    ..Default::default()
                },
                GpuTick {
                    name: "headless dGPU".into(),
                    util_pct: None,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        h.push(&snap);

        // The reporting device gets its own series; the silent one stays
        // absent so the tab can show "no live util" rather than a fake line.
        assert_eq!(
            h.gpu_util_by_name.get("Apple M3 Max").map(|r| r.to_vec()),
            Some(vec![42.0])
        );
        assert!(!h.gpu_util_by_name.contains_key("headless dGPU"));
        // Aggregate ring still carries the cross-device max.
        assert_eq!(h.gpu_util.last().copied(), Some(42.0));
    }

    #[test]
    fn gpu_util_by_name_appends_across_ticks() {
        use crate::collect::GpuTick;
        let mut h = History::new(10);
        for u in [10.0_f32, 30.0, 55.0] {
            h.push(&Snapshot {
                gpus: vec![GpuTick {
                    name: "gpu0".into(),
                    util_pct: Some(u),
                    ..Default::default()
                }],
                ..Default::default()
            });
        }
        assert_eq!(
            h.gpu_util_by_name.get("gpu0").map(|r| r.to_vec()),
            Some(vec![10.0, 30.0, 55.0])
        );
    }

    #[test]
    fn gpu_vram_by_name_records_used_fraction_only_when_reported() {
        use crate::collect::GpuTick;
        let mut h = History::new(10);
        // Tick 1: full VRAM figures → fraction recorded.
        h.push(&Snapshot {
            gpus: vec![GpuTick {
                name: "gpu0".into(),
                vram_total_bytes: Some(1000),
                vram_used_bytes: Some(250),
                ..Default::default()
            }],
            ..Default::default()
        });
        // Tick 2: total but no used → skipped (no fake sample).
        h.push(&Snapshot {
            gpus: vec![GpuTick {
                name: "gpu0".into(),
                vram_total_bytes: Some(1000),
                vram_used_bytes: None,
                ..Default::default()
            }],
            ..Default::default()
        });
        assert_eq!(
            h.gpu_vram_by_name.get("gpu0").map(|r| r.to_vec()),
            Some(vec![0.25])
        );
    }

    #[test]
    fn gpu_vram_falls_back_to_system_memory_total() {
        use crate::collect::{GpuTick, MemTick};
        let mut h = History::new(10);
        // Apple Silicon shape: no dedicated VRAM total, but used is reported,
        // and the snapshot carries total system RAM. The fraction is
        // recorded against system RAM so the history isn't perpetually empty.
        h.push(&Snapshot {
            mem: MemTick {
                total_bytes: 32 * 1024 * 1024 * 1024,
                ..Default::default()
            },
            gpus: vec![GpuTick {
                name: "Apple M3 Pro".into(),
                vram_total_bytes: None,
                vram_used_bytes: Some(8 * 1024 * 1024 * 1024),
                ..Default::default()
            }],
            ..Default::default()
        });
        assert_eq!(
            h.gpu_vram_by_name.get("Apple M3 Pro").map(|r| r.to_vec()),
            Some(vec![0.25])
        );
    }

    #[test]
    fn draw_does_not_panic_on_overview_and_gpu_with_live_gpu() {
        use crate::collect::{CpuTick, GpuTick};
        use crate::config::SyswatchConfig;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mk = |util: f32| Snapshot {
            cpu: CpuTick {
                usage_pct: 40.0,
                per_core: vec![20.0, 60.0, 90.0, 10.0],
                ..Default::default()
            },
            gpus: vec![GpuTick {
                name: "Apple M3 Max".into(),
                vendor: "Apple".into(),
                util_pct: Some(util),
                ..Default::default()
            }],
            procs: vec![proc(1, 50.0)],
            ..Default::default()
        };

        let mut app = App::new(TabId::Overview, SyswatchConfig::default());
        for u in [10.0_f32, 55.0, 80.0] {
            app.history.push(&mk(u));
        }
        let last = mk(80.0);
        app.snap = Some(last.clone());

        // Tall backend so the GPU card is big enough to carve the chart strip.
        for tab in [TabId::Overview, TabId::Gpu] {
            app.active = tab;
            let backend = TestBackend::new(120, 40);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|f| draw(f, &app, &last))
                .unwrap_or_else(|e| panic!("draw panicked on {:?}: {e}", tab));
        }
    }

    #[test]
    fn draw_does_not_panic_across_tabs_sizes_and_gpu_shapes() {
        use crate::collect::{CpuTick, GpuTick};
        use crate::config::SyswatchConfig;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let gpu = |name: &str, util: Option<f32>| GpuTick {
            name: name.into(),
            vendor: "Test".into(),
            util_pct: util,
            ..Default::default()
        };

        let gpu_sets: Vec<Vec<GpuTick>> = vec![
            vec![],
            vec![gpu("gpu0", Some(50.0))],
            vec![gpu("gpu0", None)],
            vec![gpu("iGPU", Some(20.0)), gpu("dGPU", Some(95.0))],
            vec![gpu("iGPU", Some(20.0)), gpu("dGPU", None)],
        ];

        let sizes = [(10u16, 5u16), (20, 8), (40, 12), (80, 24), (200, 60)];

        for gpus in &gpu_sets {
            let mk = || Snapshot {
                cpu: CpuTick {
                    usage_pct: 40.0,
                    per_core: vec![20.0, 60.0, 90.0, 10.0],
                    ..Default::default()
                },
                gpus: gpus.clone(),
                procs: vec![proc(1, 50.0)],
                ..Default::default()
            };
            let mut app = App::new(TabId::Overview, SyswatchConfig::default());
            for _ in 0..3 {
                app.history.push(&mk());
            }
            let last = mk();
            app.snap = Some(last.clone());

            for &tab in ALL_TABS {
                app.active = tab;
                for &(w, h) in &sizes {
                    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
                    terminal.draw(|f| draw(f, &app, &last)).unwrap_or_else(|e| {
                        panic!(
                            "draw panicked: tab={:?} size={}x{} gpus={}: {e}",
                            tab,
                            w,
                            h,
                            gpus.len()
                        )
                    });
                }
            }
        }
    }

    /// Lite writes to absolute cell coordinates rather than laying out with
    /// ratatui widgets, so an off-by-one in a column constant is a panic, not
    /// a cosmetic bug. Sweep every state at every plausible size.
    #[test]
    fn lite_draw_does_not_panic_across_sizes_and_states() {
        use crate::collect::{CpuTick, FanTick, MemTick, PowerTick, ThermalZone};
        use crate::config::SyswatchConfig;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mk = || Snapshot {
            host: crate::collect::HostInfo {
                hostname: "jules-mbp".into(),
                cpu_cores: 10,
                uptime_secs: 348_000,
                ..Default::default()
            },
            cpu: CpuTick {
                usage_pct: 40.0,
                load_1: 1.42,
                per_core: vec![20.0, 60.0, 90.0, 10.0],
                ..Default::default()
            },
            mem: MemTick {
                total_bytes: 34_359_738_368,
                used_bytes: 21_260_179_865,
                available_bytes: 13_099_558_503,
                ..Default::default()
            },
            power: PowerTick {
                thermal_zones: vec![ThermalZone {
                    name: "CPU package".into(),
                    temp_c: 52.0,
                }],
                fans: vec![FanTick {
                    name: "fan0".into(),
                    rpm: 1800,
                    target_rpm: None,
                }],
                system_power_w: Some(11.0),
                ..Default::default()
            },
            // A long CJK name and an empty one — the two shapes most likely
            // to overflow a fixed-width column.
            procs: vec![
                ProcTick {
                    pid: 1,
                    name: "firefox".into(),
                    user: "jules".into(),
                    cpu_pct: 18.4,
                    threads: Some(89),
                    ..Default::default()
                },
                ProcTick {
                    pid: 2,
                    name: "网络监视器进程网络监视器".into(),
                    user: "_windowserver".into(),
                    cpu_pct: 6.4,
                    threads: None,
                    ..Default::default()
                },
                ProcTick {
                    pid: 3,
                    name: String::new(),
                    user: String::new(),
                    cpu_pct: 0.0,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        // Below the reference grid, at it, and well beyond it.
        let sizes = [
            (10u16, 5u16),
            (40, 12),
            (79, 23),
            (80, 24),
            (81, 25),
            (200, 60),
        ];

        for &(w, h) in &sizes {
            for &(detail, filtering, filter) in &[
                (false, false, ""),
                (true, false, ""),
                (false, true, "fire"),
                (false, false, "zzz-matches-nothing"),
                (true, true, "j"),
            ] {
                let mut app = App::new(TabId::Overview, SyswatchConfig::default());
                app.view_mode = ViewMode::Lite;
                for _ in 0..3 {
                    app.history.push(&mk());
                }
                let last = mk();
                app.snap = Some(last.clone());
                app.lite.detail_open = detail;
                app.lite.filter_input = filtering;
                app.lite.filter_text = filter.into();
                // Selection past the end of a filtered list is exactly the
                // state a user reaches by filtering after scrolling down.
                app.lite.selected = 2;

                for paused in [false, true] {
                    app.paused = paused;
                    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
                    terminal.draw(|f| draw(f, &app, &last)).unwrap_or_else(|e| {
                        panic!(
                            "lite draw panicked: size={}x{} detail={} filter={:?} paused={}: {e}",
                            w, h, detail, filter, paused
                        )
                    });
                }
            }
        }
    }

    /// Toggling views must not disturb the other view's cursor, and leaving
    /// Lite must close its transient state so returning is a clean screen.
    #[test]
    fn lite_toggle_round_trips_without_leaking_state() {
        use crate::config::SyswatchConfig;
        use crossterm::event::{KeyCode, KeyEvent};

        let mut app = App::new(TabId::Procs, SyswatchConfig::default());
        app.proc_sel = 4;
        app.snap = Some(snap_with(vec![proc(1, 10.0), proc(2, 5.0)]));

        app.handle_key(KeyEvent::from(KeyCode::Char('L')));
        assert_eq!(app.view_mode, ViewMode::Lite);

        app.lite.detail_open = true;
        app.handle_key(KeyEvent::from(KeyCode::Char('L')));

        assert_eq!(app.view_mode, ViewMode::Full);
        assert!(!app.lite.detail_open, "detail should close on leaving Lite");
        assert!(!app.lite.filter_input, "filter input should close too");
        assert_eq!(app.proc_sel, 4, "the full TUI's cursor must not move");
        assert_eq!(app.active, TabId::Procs, "nor its tab");
    }

    /// While the filter box is open it owns every printable key — including
    /// the ones that are hotkeys everywhere else. Typing "L" searches for L.
    #[test]
    fn lite_filter_input_swallows_hotkeys() {
        use crate::config::SyswatchConfig;
        use crossterm::event::{KeyCode, KeyEvent};

        let mut app = App::new(TabId::Overview, SyswatchConfig::default());
        app.view_mode = ViewMode::Lite;
        app.snap = Some(snap_with(vec![proc(1, 10.0)]));
        app.handle_key(KeyEvent::from(KeyCode::Char('/')));
        assert!(app.lite.filter_input);

        for c in ['L', 'p', 'q', '?'] {
            app.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
        assert_eq!(app.lite.filter_text, "Lpq?");
        assert_eq!(app.view_mode, ViewMode::Lite, "L must not have toggled");
        assert!(!app.paused, "p must not have paused");
        assert!(!app.help_active, "? must not have opened help");
    }

    /// `p` pauses from either view — the two share one collector, so the
    /// pause has to be the same pause.
    #[test]
    fn lite_pause_is_the_app_pause() {
        use crate::config::SyswatchConfig;
        use crossterm::event::{KeyCode, KeyEvent};

        let mut app = App::new(TabId::Overview, SyswatchConfig::default());
        app.view_mode = ViewMode::Lite;
        app.handle_key(KeyEvent::from(KeyCode::Char('p')));
        assert!(app.paused);
        app.handle_key(KeyEvent::from(KeyCode::Char('p')));
        assert!(!app.paused);
    }

    /// Opening detail expands the selected row; it must never reassign the
    /// selection to make room (the design draft's "moves to index 1").
    #[test]
    fn lite_detail_does_not_move_the_selection() {
        use crate::config::SyswatchConfig;
        use crossterm::event::{KeyCode, KeyEvent};

        let mut app = App::new(TabId::Overview, SyswatchConfig::default());
        app.view_mode = ViewMode::Lite;
        app.last_area = Rect::new(0, 0, 80, 24);
        app.snap = Some(snap_with(
            (0..8).map(|i| proc(i, 10.0 - i as f32)).collect(),
        ));

        for _ in 0..3 {
            app.handle_key(KeyEvent::from(KeyCode::Down));
        }
        assert_eq!(app.lite.selected, 3);

        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(app.lite.detail_open);
        assert_eq!(app.lite.selected, 3, "detail must not move the selection");

        // Selecting deep into the list with detail open scrolls instead.
        for _ in 0..4 {
            app.handle_key(KeyEvent::from(KeyCode::Down));
        }
        assert_eq!(app.lite.selected, 7);
        let visible = crate::ui::lite::Layout::new(app.last_area).visible_procs(true) as usize;
        assert!(
            app.lite.selected < app.lite.offset + visible,
            "selection {} scrolled out of the {}-row window at offset {}",
            app.lite.selected,
            visible,
            app.lite.offset
        );
    }

    /// Esc unwinds one layer at a time rather than dumping every bit of
    /// state at once — the same behaviour NetWatch Lite has.
    #[test]
    fn lite_esc_unwinds_one_layer_at_a_time() {
        use crate::config::SyswatchConfig;
        use crossterm::event::{KeyCode, KeyEvent};

        let mut app = App::new(TabId::Overview, SyswatchConfig::default());
        app.view_mode = ViewMode::Lite;
        app.snap = Some(snap_with(vec![proc(1, 10.0)]));
        app.lite.filter_text = "fire".into();
        app.lite.detail_open = true;

        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(!app.lite.detail_open);
        assert_eq!(
            app.lite.filter_text, "fire",
            "filter survives the first Esc"
        );

        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(app.lite.filter_text.is_empty());
    }

    #[test]
    #[ignore = "samples the live machine; run explicitly"]
    fn lite_renders_live_sample() {
        use crate::config::SyswatchConfig;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut c = Collector::new(1000);
        let mut app = App::new(TabId::Overview, SyswatchConfig::default());
        app.view_mode = ViewMode::Lite;
        let mut last = c.sample();
        for _ in 0..3 {
            std::thread::sleep(Duration::from_millis(400));
            last = c.sample();
            app.history.push(&last);
            let sr = crate::ui::lite::swap_rate(&app.history, 400);
            app.lite.alerts.update(&last, sr);
        }
        app.snap = Some(last.clone());

        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| draw(f, &app, &last)).unwrap();
        let buf = term.backend().buffer();
        let rows: Vec<String> = (0..24)
            .map(|y| {
                (0..80)
                    .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                    .collect::<String>()
            })
            .collect();
        for r in &rows {
            println!("|{}", r.trim_end());
        }

        // Real data must actually reach the screen — a Lite that renders its
        // chrome perfectly around empty fields is the failure mode a
        // fixture-driven test can't see.
        assert!(rows[0].starts_with(" syswatch  "), "header: {:?}", rows[0]);
        assert!(
            rows[0].contains(&last.host.hostname),
            "hostname missing: {:?}",
            rows[0]
        );
        assert!(rows[2].contains("% cpu"), "cpu label: {:?}", rows[2]);
        assert!(rows[6].contains("GB /"), "mem label: {:?}", rows[6]);
        assert!(rows[9].contains("ago") && rows[9].contains("now"));
        assert!(rows[10].starts_with(" temp "), "vitals: {:?}", rows[10]);
        for (k, label) in crate::ui::lite::FOOTER_KEYS {
            assert!(rows[23].contains(label), "footer missing {} {}", k, label);
        }
        // The live machine has processes; at least the first row must be one.
        assert!(
            !rows[14].trim().is_empty(),
            "no process rows rendered from a live sample"
        );
    }

    #[test]
    fn session_mirrors_snapshots_into_ring() {
        let mut h = History::new(3);
        for cpu in [10.0, 20.0, 30.0, 40.0_f32] {
            h.push(&snap_with(vec![proc(1, cpu)]));
        }
        // Cap=3 → drops the oldest (10.0).
        let session = h.session.to_vec();
        assert_eq!(session.len(), 3);
        assert_eq!(session[0].procs[0].cpu_pct, 20.0);
        assert_eq!(session[2].procs[0].cpu_pct, 40.0);
    }

    // ── proc filter keybinding (issue #18) ──────────────────────────────

    fn press(app: &mut App, c: char) {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }

    #[test]
    fn slash_and_f_both_open_the_filter_on_every_filterable_tab() {
        // On layouts like ergol, `/` sits on the AltGr layer and can be
        // unreachable, so `f` is an equivalent trigger (issue #18). Both
        // must work on every tab that has a table to search — the reporter
        // of issue #20 hit Memory, where only Procs was wired up.
        for tab in [TabId::Procs, TabId::Memory, TabId::Services] {
            for key in ['/', 'f'] {
                let mut app = App::new(tab, SyswatchConfig::default());
                press(&mut app, key);
                assert!(
                    app.filter_input,
                    "'{key}' should open the filter on {}",
                    tab.title()
                );
            }
        }
    }

    #[test]
    fn filter_trigger_is_inert_on_unfilterable_tabs() {
        // Tabs with no table to narrow must stay inert — and must not
        // advertise the key either, which is checked by pairing the
        // handler against the same `supports_filter` the footer reads.
        for tab in [TabId::Overview, TabId::Cpu, TabId::Disks, TabId::Timeline] {
            assert!(!tab.supports_filter(), "{} should not filter", tab.title());
            let mut app = App::new(tab, SyswatchConfig::default());
            press(&mut app, '/');
            press(&mut app, 'f');
            assert!(!app.filter_input, "{} opened a filter", tab.title());
        }
    }

    #[test]
    fn filter_carries_across_filterable_tabs() {
        // One shared filter: typing it on Procs and switching to Memory
        // keeps the narrowing, rather than silently resetting.
        let mut app = App::new(TabId::Procs, SyswatchConfig::default());
        press(&mut app, '/');
        press(&mut app, 's');
        press(&mut app, 'h');
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.filter_active.as_deref(), Some("sh"));
        press(&mut app, '3'); // Memory
        assert_eq!(app.active, TabId::Memory);
        assert_eq!(app.filter_active.as_deref(), Some("sh"));
    }

    #[test]
    fn changing_the_filter_resets_both_table_selections() {
        // The filter is shared, so a stale cursor on the *other* tab is
        // just as out of bounds as one on the tab in view.
        let mut app = App::new(TabId::Services, SyswatchConfig::default());
        app.proc_sel = 12;
        app.service_sel = 9;
        press(&mut app, '/');
        press(&mut app, 'x');
        assert_eq!(app.proc_sel, 0);
        assert_eq!(app.service_sel, 0);
    }

    #[test]
    fn f_types_into_the_filter_buffer_once_editing() {
        // The `f` alias must not swallow a literal 'f' the user types as
        // part of a filter term — input mode routes chars to the buffer.
        let mut app = App::new(TabId::Procs, SyswatchConfig::default());
        press(&mut app, '/');
        assert!(app.filter_input);
        press(&mut app, 'f');
        assert_eq!(app.filter_buf, "f");
    }
}
