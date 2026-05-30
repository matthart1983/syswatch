use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{App, Snapshot};
use crate::collect::GpuTick;
use crate::ui::{
    graph::{self, GraphStyle},
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

/// Rows reserved at the bottom of a card for the util-history strip (one
/// label row + the chart). Only carved out when the card is tall enough that
/// the metrics/status block above it still gets its minimum height.
const CHART_H: u16 = 4;

fn draw_card(f: &mut Frame, area: Rect, gpu: &GpuTick, app: &App, snap: &Snapshot) {
    let style = app.graph_style;
    let title = format!("[{}] {}", gpu.vendor, gpu.name);
    let block = panel(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Reserve a util-history strip at the bottom when this device reports
    // live util, has at least two samples to draw a line from, and the card
    // is tall enough that the metrics/status split keeps its minimum height.
    // Otherwise the card keeps its original full-height two-column layout.
    let history = app.history.gpu_util_by_name.get(&gpu.name);
    let want_chart = gpu.util_pct.is_some()
        && history.map(|r| r.len() >= 2).unwrap_or(false)
        && inner.height >= CHART_H + 5;

    let (top, chart) = if want_chart {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(5), Constraint::Length(CHART_H)])
            .split(inner);
        (rows[0], Some(rows[1]))
    } else {
        (inner, None)
    };

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(top);

    draw_metrics(f, cols[0], gpu, style);
    draw_status(f, cols[1], gpu, snap);

    if let (Some(chart_area), Some(series)) = (chart, history) {
        draw_util_history(f, chart_area, &series.to_vec(), style, app.graph_opts());
    }
}

/// Time-series of this device's util %, mirroring the CPU tab's aggregate
/// strip: a muted label row over a single chart fed through the shared graph
/// renderer (so the `g` bars/dots toggle and btop fade apply here too).
fn draw_util_history(
    f: &mut Frame,
    area: Rect,
    series: &[f32],
    style: GraphStyle,
    opts: crate::ui::graph::GraphOpts,
) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);

    let cur = series.last().copied().unwrap_or(0.0);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("util ", Style::default().fg(p::text_muted())),
            Span::styled(
                format!("now {:.0}% · {} samples", cur, series.len()),
                Style::default().fg(p::text_muted()),
            ),
        ]))
        .style(Style::default().bg(p::bg())),
        rows[0],
    );

    let normalized: Vec<f32> = series.iter().map(|v| (v / 100.0).clamp(0.0, 1.0)).collect();
    graph::render(f, rows[1], &normalized, style, p::brand(), opts);
}

fn draw_metrics(f: &mut Frame, area: Rect, gpu: &GpuTick, style: GraphStyle) {
    let mut lines: Vec<Line> = Vec::new();

    // Util bar.
    let util_label = match gpu.util_pct {
        Some(u) => format!("util   {:>5.1}%", u),
        None => "util   —".into(),
    };
    let util_color = util_color(gpu.util_pct.unwrap_or(0.0));
    lines.push(Line::from(vec![Span::styled(
        util_label,
        Style::default()
            .fg(if gpu.util_pct.is_some() {
                util_color
            } else {
                p::text_muted()
            })
            .add_modifier(Modifier::BOLD),
    )]));
    if let Some(u) = gpu.util_pct {
        let bar = block_bar_styled(u / 100.0, area.width.saturating_sub(2), util_color, style);
        lines.push(bar);
    } else {
        lines.push(Line::from(vec![Span::styled(
            std::iter::repeat('\u{2500}')
                .take(area.width.saturating_sub(2) as usize)
                .collect::<String>(),
            Style::default().fg(p::border()),
        )]));
    }

    // Apple-Silicon-only renderer / tiler split. Two slim bars half-width
    // each, side-by-side under the main util bar. Skipped silently on
    // platforms that don't expose the breakdown.
    if let (Some(r), Some(t)) = (gpu.renderer_util_pct, gpu.tiler_util_pct) {
        let total_w = area.width.saturating_sub(2) as usize;
        // 4 chars of label per side ("R: " + " "), gap of 2 between halves.
        let bar_w = total_w.saturating_sub(4 + 4 + 2) / 2;
        let r_bar = block_bar_styled(r / 100.0, bar_w as u16, p::brand(), style);
        let t_bar = block_bar_styled(t / 100.0, bar_w as u16, p::tx_rate(), style);
        let mut spans: Vec<Span> = Vec::new();
        spans.push(Span::styled(
            format!(" R{:>3.0}% ", r),
            Style::default().fg(p::text_muted()),
        ));
        spans.extend(r_bar.spans);
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("T{:>3.0}% ", t),
            Style::default().fg(p::text_muted()),
        ));
        spans.extend(t_bar.spans);
        lines.push(Line::from(spans));
    }

    lines.push(Line::from(""));

    // VRAM gauge.
    match (gpu.vram_total_bytes, gpu.vram_used_bytes) {
        (Some(total), Some(used)) => {
            let frac = used as f32 / total.max(1) as f32;
            lines.push(Line::from(vec![Span::styled(
                format!("vram   {} / {}", human_bytes(used), human_bytes(total)),
                Style::default().fg(p::brand()).add_modifier(Modifier::BOLD),
            )]));
            lines.push(block_bar_styled(
                frac,
                area.width.saturating_sub(2),
                p::brand(),
                style,
            ));
        }
        (Some(total), None) => {
            lines.push(Line::from(vec![Span::styled(
                format!("vram   {} (used: —)", human_bytes(total)),
                Style::default().fg(p::text_muted()),
            )]));
        }
        _ => {
            lines.push(Line::from(vec![Span::styled(
                "vram   —",
                Style::default().fg(p::text_muted()),
            )]));
        }
    }

    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(p::bg())),
        area,
    );
}

fn draw_status(f: &mut Frame, area: Rect, gpu: &GpuTick, snap: &Snapshot) {
    let mut lines: Vec<Line> = Vec::new();

    lines.push(kv("vendor", gpu.vendor.clone(), p::text_primary()));
    if let Some(d) = &gpu.driver {
        lines.push(kv("driver", d.clone(), p::text_muted()));
    }
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

    // macOS-only "last submitter" hint — rotating PID from ioreg's
    // AGCInfo dict. Honest about its limitation: this is the most
    // recent process to submit GPU work, not a usage column.
    if let Some(pid) = gpu.last_submitter_pid {
        let name = snap
            .procs
            .iter()
            .find(|p| p.pid == pid)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "?".into());
        lines.push(kv(
            "last sub",
            format!("{} (pid {})", name, pid),
            p::text_primary(),
        ));
    }

    if let Some(hint) = &gpu.live_data_hint {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            "live data",
            Style::default()
                .fg(p::status_warn())
                .add_modifier(Modifier::BOLD),
        )]));
        // Wrap the hint over multiple lines if needed.
        let mut s = hint.as_str();
        let max_w = area.width.saturating_sub(2) as usize;
        while !s.is_empty() {
            let take = s.len().min(max_w);
            let mut split_at = take;
            if take < s.len() {
                if let Some(last_space) = s[..take].rfind(' ') {
                    split_at = last_space;
                }
            }
            let (head, rest) = s.split_at(split_at);
            lines.push(Line::from(vec![Span::styled(
                head.trim_end().to_string(),
                Style::default().fg(p::text_muted()),
            )]));
            s = rest.trim_start();
        }
    }

    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(p::bg())),
        area,
    );
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
    Line::from(vec![
        Span::styled(format!("{:<8} ", k), Style::default().fg(p::text_muted())),
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

    #[test]
    fn util_history_renders_current_value_and_chart_glyphs() {
        // Five samples ending at 30% — the label row must surface the latest
        // value and the sample count, and the chart row must draw block-bar
        // glyphs from the shared renderer (proves the series reached it).
        let series = vec![10.0_f32, 50.0, 90.0, 70.0, 30.0];
        let backend = TestBackend::new(40, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_util_history(f, f.area(), &series, GraphStyle::Bars, GraphOpts::default());
            })
            .unwrap();

        let text = buffer_to_string(terminal.backend().buffer());
        assert!(text.contains("now 30%"), "missing current util:\n{text}");
        assert!(text.contains("5 samples"), "missing sample count:\n{text}");
        assert!(
            text.chars().any(|c| ('\u{2581}'..='\u{2588}').contains(&c)),
            "expected block-bar glyphs in the chart row:\n{text}"
        );
    }
}
