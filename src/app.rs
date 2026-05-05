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
use crate::insights::{self, Insight};
use crate::tabs;
use crate::ui::chrome;
use crate::ui::graph::GraphStyle;

pub struct Options {
    pub tick_ms: u64,
    pub start_tab: Option<String>,
}

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
    /// Per-pid CPU EWMA, decayed each tick. Pids absent in the latest tick
    /// are pruned. Values are 0..100. The runaway-proc heuristic reads this
    /// to find processes whose load is sustained, not transient.
    pub proc_cpu_ewma: HashMap<u32, f32>,
    /// Full session: every snapshot pushed in order. Bounded — sized to
    /// match the metric rings so scrubbing stays in sync. The Timeline tab
    /// drives scrubbing; other tabs read App::displayed_snap().
    pub session: Ring<Snapshot>,
}

impl History {
    pub(crate) fn new(cap: usize) -> Self {
        Self {
            cpu: Ring::new(cap),
            mem: Ring::new(cap),
            swap: Ring::new(cap),
            net_rate: Ring::new(cap),
            io_rate: Ring::new(cap),
            proc_cpu_ewma: HashMap::new(),
            session: Ring::new(cap),
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
    Rss,
    Io,
    Start,
    Name,
}

impl ProcSort {
    pub fn label(&self) -> &'static str {
        match self {
            ProcSort::Cpu => "cpu",
            ProcSort::Rss => "rss",
            ProcSort::Io => "io",
            ProcSort::Start => "start",
            ProcSort::Name => "name",
        }
    }
    pub const ALL: [ProcSort; 5] = [
        ProcSort::Cpu,
        ProcSort::Rss,
        ProcSort::Io,
        ProcSort::Start,
        ProcSort::Name,
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
}

pub struct App {
    pub active: TabId,
    pub paused: bool,
    pub history: History,
    pub snap: Option<Snapshot>,
    pub proc_sort: ProcSort,
    pub proc_sel: usize,
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
}

impl App {
    pub fn displayed_snap(&self) -> Option<&Snapshot> {
        if self.scrub_offset > 0 {
            self.history.session.nth_back(self.scrub_offset)
        } else {
            self.snap.as_ref()
        }
    }
    pub fn live_state(&self) -> LiveState {
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
    fn new(start: TabId) -> Self {
        Self {
            active: start,
            paused: false,
            history: History::new(120),
            snap: None,
            proc_sort: ProcSort::Cpu,
            proc_sel: 0,
            service_sort: ServiceSort::Name,
            service_sel: 0,
            insights: Vec::new(),
            scrub_offset: 0,
            graph_style: GraphStyle::Bars,
        }
    }

    fn handle_key(&mut self, k: KeyEvent) -> bool {
        if k.kind != KeyEventKind::Press {
            return false;
        }
        match (k.code, k.modifiers) {
            (KeyCode::Char('q'), _) => return true,
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
            (KeyCode::Char('p'), _) => self.paused = !self.paused,
            (KeyCode::Char('g'), _) => self.graph_style = self.graph_style.next(),
            (KeyCode::Char('t'), _) => {
                crate::ui::theme::cycle();
            }
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
                let max = self
                    .snap
                    .as_ref()
                    .map(|s| s.procs.len().saturating_sub(1))
                    .unwrap_or(0);
                self.proc_sel = (self.proc_sel + 1).min(max);
            }
            (KeyCode::Char('s'), _) if self.active == TabId::Procs => {
                self.proc_sort = self.proc_sort.next();
                self.proc_sel = 0;
            }
            (KeyCode::Up, _) if self.active == TabId::Services => {
                self.service_sel = self.service_sel.saturating_sub(1);
            }
            (KeyCode::Down, _) if self.active == TabId::Services => {
                let max = self
                    .snap
                    .as_ref()
                    .map(|s| s.services.len().saturating_sub(1))
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

fn next_tab(t: TabId) -> TabId {
    let i = ALL_TABS.iter().position(|x| *x == t).unwrap_or(0);
    ALL_TABS[(i + 1) % ALL_TABS.len()]
}

fn prev_tab(t: TabId) -> TabId {
    let i = ALL_TABS.iter().position(|x| *x == t).unwrap_or(0);
    ALL_TABS[(i + ALL_TABS.len() - 1) % ALL_TABS.len()]
}

pub fn run(opts: Options) -> Result<()> {
    let start = opts
        .start_tab
        .as_deref()
        .and_then(TabId::from_str_loose)
        .unwrap_or(TabId::Overview);
    let mut app = App::new(start);
    let mut collector = Collector::new();

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut term = Terminal::new(backend)?;

    let tick = Duration::from_millis(opts.tick_ms.max(100));
    let mut last_tick = Instant::now() - tick; // force immediate sample
    let res = loop {
        if last_tick.elapsed() >= tick {
            if !app.paused {
                let s = collector.sample();
                app.history.push(&s);
                app.insights = insights::compute(&app.history, &s);
                app.snap = Some(s);
            }
            last_tick = Instant::now();
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
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Length(2), // tab bar (label + underline)
            Constraint::Min(0),    // body
            Constraint::Length(2), // footer (separator + hotkeys)
        ])
        .split(area);

    chrome::draw_header(f, chunks[0], snap, app.live_state());
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
    chrome::draw_footer(f, chunks[3], app.graph_style);
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
}
