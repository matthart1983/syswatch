use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{App, Snapshot};
use crate::collect::GpuTick;
use crate::ui::{
    graph::{self, GraphOpts, GraphStyle},
    palette as p,
    widgets::{block_bar_styled, human_bytes, panel},
};

pub fn draw(f: &mut Frame, area: Rect, app: &App, snap: &Snapshot) {
    if snap.gpus.is_empty() {
        draw_empty(f, area);
        return;
    }

    let n = snap.gpus.len() as u16;
    let card_h = (area.height / n).max(7);
    let constraints: Vec<Constraint> = (0..n).map(|_| Constraint::Length(card_h)).collect();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    for (i, gpu) in snap.gpus.iter().enumerate() {
        if let Some(rect) = chunks.get(i) {
            draw_card(f, *rect, gpu, app, snap);
        }
    }
}

/// Rows for the per-engine section: one label row + a bar per engine
/// (renderer + tiler = 2) + a blank separator.
const ENGINE_H: u16 = 4;

fn draw_card(f: &mut Frame, area: Rect, gpu: &GpuTick, app: &App, snap: &Snapshot) {
    // Mirror the CPU tab's two-section layout. Top: a "(last ~120s)" panel
    // with a wide util-history chart and a counters column — the GPU analog
    // of CPU's aggregate panel. Bottom (Apple Silicon only): a per-engine
    // section with renderer/tiler bars — the analog of CPU's per-core block.
    let title = format!("[{}] {} (last ~120s)", gpu.vendor, gpu.name);
    let block = panel(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Carve off the per-engine strip when this device exposes the breakdown
    // and the card is tall enough to keep a usable chart above it. On
    // platforms without the split (or in short multi-GPU cards) the chart +
    // counters take the whole card.
    let has_engines = gpu.renderer_util_pct.is_some() && gpu.tiler_util_pct.is_some();
    let (top, engines) = if has_engines && inner.height >= ENGINE_H + 5 {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(5), Constraint::Length(ENGINE_H)])
            .split(inner);
        (rows[0], Some(rows[1]))
    } else {
        (inner, None)
    };

    // Fixed-width status column (instead of a percentage) so its longest
    // rows — driver string, last-submitter "name (pid)" — stay readable
    // without clipping, and the charts take all remaining width.
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(20), Constraint::Length(30)])
        .split(top);

    // Left column: two stacked time-series — util on top, VRAM below — each a
    // labeled chart. Right column: the remaining scalar counters.
    let chart_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)])
        .split(cols[0]);

    draw_series(f, chart_rows[0], &util_spec(gpu, app), app.graph_style, app.graph_opts());
    draw_series(f, chart_rows[1], &vram_spec(gpu, app), app.graph_style, app.graph_opts());
    draw_counters(f, cols[1], gpu, snap);

    if let Some(engine_area) = engines {
        draw_engines(f, engine_area, gpu, app.graph_style);
    }
}

/// One labeled time-series pane: a `label   value` header row over a chart,
/// or the placeholder line when there aren't yet two samples to draw. Used
/// for both the util and VRAM histories so they render identically.
struct ChartSpec<'a> {
    label: &'a str,
    value: String,
    value_color: Color,
    line_color: Color,
    /// Pre-normalized to 0..=1.
    series: Vec<f32>,
    placeholder: &'a str,
}

fn util_spec<'a>(gpu: &'a GpuTick, app: &App) -> ChartSpec<'a> {
    let series = app
        .history
        .gpu_util_by_name
        .get(&gpu.name)
        .map(|r| r.to_vec().iter().map(|v| (v / 100.0).clamp(0.0, 1.0)).collect())
        .unwrap_or_default();
    let placeholder = gpu.live_data_hint.as_deref().unwrap_or(if gpu.util_pct.is_some() {
        "collecting util…"
    } else {
        "no live util on this device"
    });
    ChartSpec {
        label: "util",
        value: gpu
            .util_pct
            .map(|u| format!("{:>5.1}%", u))
            .unwrap_or_else(|| "—".into()),
        value_color: gpu.util_pct.map(util_color).unwrap_or(p::text_muted()),
        line_color: p::brand(),
        series,
        placeholder,
    }
}

fn vram_spec<'a>(gpu: &'a GpuTick, app: &App) -> ChartSpec<'a> {
    let series = app
        .history
        .gpu_vram_by_name
        .get(&gpu.name)
        .map(|r| r.to_vec())
        .unwrap_or_default();
    ChartSpec {
        label: "vram",
        value: vram_value(gpu),
        value_color: p::brand(),
        line_color: p::tx_rate(),
        series,
        placeholder: "no vram usage data",
    }
}

/// VRAM chart header: `used / total (pct%)`, degrading gracefully when only
/// some of the figures are available.
fn vram_value(gpu: &GpuTick) -> String {
    match (gpu.vram_total_bytes, gpu.vram_used_bytes) {
        (Some(total), Some(used)) if total > 0 => format!(
            "{} / {} ({:.0}%)",
            human_bytes(used),
            human_bytes(total),
            100.0 * used as f32 / total as f32
        ),
        (Some(total), Some(used)) => format!("{} / {}", human_bytes(used), human_bytes(total)),
        (Some(total), None) => format!("{} (used —)", human_bytes(total)),
        _ => "—".into(),
    }
}

fn draw_series(f: &mut Frame, area: Rect, spec: &ChartSpec, style: GraphStyle, opts: GraphOpts) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!("{:<7}", spec.label),
                Style::default().fg(p::text_muted()),
            ),
            Span::styled(spec.value.clone(), Style::default().fg(spec.value_color)),
        ]))
        .style(Style::default().bg(p::bg())),
        rows[0],
    );

    if spec.series.len() >= 2 {
        graph::render(f, rows[1], &spec.series, style, spec.line_color, opts);
    } else {
        f.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                spec.placeholder,
                Style::default().fg(p::text_muted()),
            )]))
            .style(Style::default().bg(p::bg())),
            rows[1],
        );
    }
}

/// Bottom pane: per-engine utilization bars, the GPU analog of the CPU tab's
/// per-core block. Apple Silicon exposes a renderer (fragment-shader) and
/// tiler (fixed-function rasterizer) split that roughly sums to total util.
fn draw_engines(f: &mut Frame, area: Rect, gpu: &GpuTick, style: GraphStyle) {
    let block = panel("per-engine");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    for (label, val) in [
        ("render", gpu.renderer_util_pct),
        ("tiler", gpu.tiler_util_pct),
    ] {
        let Some(v) = val else { continue };
        let color = util_color(v);
        // label (7) + " " + " 100.0%" suffix (7) = 15 reserved for text.
        let bar = block_bar_styled(v / 100.0, inner.width.saturating_sub(15), color, style);
        let mut spans = vec![Span::styled(
            format!("{:<7}", label),
            Style::default().fg(p::text_muted()),
        )];
        spans.extend(bar.spans);
        spans.push(Span::styled(
            format!(" {:>5.1}%", v),
            Style::default()
                .fg(p::text_primary())
                .add_modifier(Modifier::BOLD),
        ));
        lines.push(Line::from(spans));
    }

    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(p::bg())),
        inner,
    );
}

/// Right pane: scalar readouts as a `kv` list, matching the CPU tab's
/// counters column. `util` and `vram` live as the stacked chart headers on
/// the left, so these counters carry the remaining current values: temp,
/// power, driver, and the macOS last-submitter hint.
fn draw_counters(f: &mut Frame, area: Rect, gpu: &GpuTick, snap: &Snapshot) {
    let block = panel("status");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();

    lines.push(kv(
        "temp",
        gpu.temp_c
            .map(|t| format!("{:.0}°C", t))
            .unwrap_or_else(|| "—".into()),
        gpu.temp_c
            .map(|t| {
                if t >= 80.0 {
                    p::status_error()
                } else if t >= 70.0 {
                    p::status_warn()
                } else {
                    p::status_good()
                }
            })
            .unwrap_or(p::text_muted()),
    ));

    lines.push(kv(
        "power",
        gpu.power_w
            .map(|w| format!("{:.1} W", w))
            .unwrap_or_else(|| "—".into()),
        if gpu.power_w.is_some() {
            p::text_primary()
        } else {
            p::text_muted()
        },
    ));

    if let Some(d) = &gpu.driver {
        lines.push(kv("driver", d.clone(), p::text_muted()));
    }

    // macOS-only "last submitter" — rotating PID from ioreg's AGCInfo dict.
    // The most recent process to submit GPU work, not a usage column.
    if let Some(pid) = gpu.last_submitter_pid {
        let name = snap
            .procs
            .iter()
            .find(|p| p.pid == pid)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "?".into());
        // Keep the process name from blowing out the narrow column; the pid
        // is the part you copy, so it always stays visible.
        let name = truncate(&name, 14);
        lines.push(kv("last sub", format!("{} ({})", name, pid), p::text_primary()));
    }

    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(p::bg())),
        inner,
    );
}

/// Truncate to `max` chars with an ellipsis, counting by char (not byte) so
/// multibyte process names don't panic on a byte split.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

fn draw_empty(f: &mut Frame, area: Rect) {
    let block = panel("GPU");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let lines = vec![
        Line::from(vec![Span::styled(
            "No GPUs detected",
            Style::default()
                .fg(p::text_muted())
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from(""),
        Line::from(vec![Span::styled(
            "Discovery probes:",
            Style::default().fg(p::text_muted()),
        )]),
        Line::from(vec![Span::styled(
            "  macOS  →  system_profiler SPDisplaysDataType -json",
            Style::default().fg(p::border()),
        )]),
        Line::from(vec![Span::styled(
            "  Linux  →  /sys/class/drm/card*/device/{vendor,device}",
            Style::default().fg(p::border()),
        )]),
    ];
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(p::bg())),
        inner,
    );
}

fn kv(k: &str, v: String, val_color: ratatui::style::Color) -> Line<'static> {
    // 9-wide key field guarantees a gap even for the longest key ("last sub"
    // is 8 chars, which `{:<8}` would butt straight against its value).
    Line::from(vec![
        Span::styled(format!("{:<9}", k), Style::default().fg(p::text_muted())),
        Span::styled(v, Style::default().fg(val_color)),
    ])
}

fn util_color(u: f32) -> ratatui::style::Color {
    if u >= 85.0 {
        p::status_error()
    } else if u >= 60.0 {
        p::status_warn()
    } else {
        p::status_good()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn util_color_thresholds() {
        assert_eq!(util_color(0.0), p::status_good());
        assert_eq!(util_color(59.9), p::status_good());
        assert_eq!(util_color(60.0), p::status_warn());
        assert_eq!(util_color(84.9), p::status_warn());
        assert_eq!(util_color(85.0), p::status_error());
        assert_eq!(util_color(100.0), p::status_error());
    }

    use crate::ui::graph::GraphOpts;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn buffer_to_string(buf: &ratatui::buffer::Buffer) -> String {
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn spec(label: &'static str, value: &str, series: Vec<f32>, placeholder: &'static str) -> ChartSpec<'static> {
        ChartSpec {
            label,
            value: value.to_string(),
            value_color: p::text_primary(),
            line_color: p::brand(),
            series,
            placeholder,
        }
    }

    #[test]
    fn series_renders_label_value_and_block_glyphs() {
        // A normalized multi-sample series must show its label + value header
        // and reach the shared renderer to draw block-bar glyphs.
        let s = spec("util", "30.0%", vec![0.1, 0.5, 0.9, 0.7, 0.3], "—");
        let backend = TestBackend::new(40, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_series(f, f.area(), &s, GraphStyle::Bars, GraphOpts::default()))
            .unwrap();

        let text = buffer_to_string(terminal.backend().buffer());
        assert!(text.contains("util"), "missing label:\n{text}");
        assert!(text.contains("30.0%"), "missing value:\n{text}");
        assert!(
            text.chars().any(|c| ('\u{2581}'..='\u{2588}').contains(&c)),
            "expected block-bar glyphs in the chart pane:\n{text}"
        );
    }

    #[test]
    fn series_shows_placeholder_when_no_samples() {
        // Fewer than two samples → render the placeholder, not an empty chart.
        let s = spec("vram", "—", vec![], "no vram usage data");
        let backend = TestBackend::new(40, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_series(f, f.area(), &s, GraphStyle::Bars, GraphOpts::default()))
            .unwrap();

        let text = buffer_to_string(terminal.backend().buffer());
        assert!(
            text.contains("no vram usage data"),
            "missing no-data placeholder:\n{text}"
        );
    }

    #[test]
    fn vram_value_formats_used_total_and_percent() {
        // The VRAM chart header summarizes used / total and the percentage.
        let gpu = GpuTick {
            vram_total_bytes: Some(48 * 1024 * 1024 * 1024),
            vram_used_bytes: Some(12 * 1024 * 1024 * 1024),
            ..Default::default()
        };
        assert!(
            vram_value(&gpu).contains("(25%)"),
            "expected 25% in {:?}",
            vram_value(&gpu)
        );
        // Total but no used → honest about the missing figure.
        let partial = GpuTick {
            vram_total_bytes: Some(1024),
            vram_used_bytes: None,
            ..Default::default()
        };
        assert!(vram_value(&partial).contains("used —"));
    }

    #[test]
    fn counters_surface_temp_and_power() {
        // util + vram now live as chart headers; the counters carry the
        // remaining scalars — temp and power.
        let gpu = GpuTick {
            vendor: "Apple".into(),
            temp_c: Some(65.0),
            power_w: Some(12.5),
            ..Default::default()
        };
        let snap = Snapshot::default();
        let backend = TestBackend::new(34, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_counters(f, f.area(), &gpu, &snap))
            .unwrap();

        let text = buffer_to_string(terminal.backend().buffer());
        assert!(text.contains("65°C"), "missing temp value:\n{text}");
        assert!(text.contains("12.5 W"), "missing power value:\n{text}");
    }

    #[test]
    fn engines_render_renderer_and_tiler_bars() {
        // The per-engine section labels both engines, shows their values, and
        // draws a bar (block glyph) for each — the GPU analog of per-core.
        let gpu = GpuTick {
            renderer_util_pct: Some(38.0),
            tiler_util_pct: Some(4.0),
            ..Default::default()
        };
        let backend = TestBackend::new(40, 4);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_engines(f, f.area(), &gpu, GraphStyle::Bars))
            .unwrap();

        let text = buffer_to_string(terminal.backend().buffer());
        assert!(text.contains("per-engine"), "missing section label:\n{text}");
        assert!(text.contains("render"), "missing renderer row:\n{text}");
        assert!(text.contains("tiler"), "missing tiler row:\n{text}");
        assert!(text.contains("38.0%"), "missing renderer value:\n{text}");
        assert!(
            text.chars().any(|c| ('\u{2581}'..='\u{2588}').contains(&c)),
            "expected bar glyphs for the engines:\n{text}"
        );
    }
}
