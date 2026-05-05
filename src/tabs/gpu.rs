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
    graph::GraphStyle,
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
            draw_card(f, *rect, gpu, app.graph_style);
        }
    }
}

fn draw_card(f: &mut Frame, area: Rect, gpu: &GpuTick, style: GraphStyle) {
    let title = format!("[{}] {}", gpu.vendor, gpu.name);
    let block = panel(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(inner);

    draw_metrics(f, cols[0], gpu, style);
    draw_status(f, cols[1], gpu);
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

fn draw_status(f: &mut Frame, area: Rect, gpu: &GpuTick) {
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
        if gpu.power_w.is_some() { p::text_primary() } else { p::text_muted() },
    ));

    if let Some(hint) = &gpu.live_data_hint {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            "live data",
            Style::default().fg(p::status_warn()).add_modifier(Modifier::BOLD),
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
            Style::default().fg(p::text_muted()).add_modifier(Modifier::BOLD),
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
