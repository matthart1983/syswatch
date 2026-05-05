use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{App, Snapshot};
use crate::ui::{
    graph::{self, GraphStyle},
    palette as p,
    widgets::{block_bar_styled, panel},
};

pub fn draw(f: &mut Frame, area: Rect, app: &App, snap: &Snapshot) {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(8), Constraint::Min(0)])
        .split(area);

    draw_aggregate(f, v[0], app, snap);
    draw_per_core(f, v[1], snap, app.graph_style);
}

fn draw_aggregate(f: &mut Frame, area: Rect, app: &App, snap: &Snapshot) {
    let block = panel("Aggregate CPU (last ~120s)");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(inner);

    // Chart strip.
    let series = app.history.cpu.to_vec();
    let normalized: Vec<f32> = series.iter().map(|v| (v / 100.0).clamp(0.0, 1.0)).collect();

    let spark_area = Rect {
        x: cols[0].x,
        y: cols[0].y + 1,
        width: cols[0].width,
        height: cols[0].height.saturating_sub(2),
    };
    graph::render(f, spark_area, &normalized, app.graph_style, p::brand());

    // Counters panel.
    let counters = vec![
        kv("usage", format!("{:>5.1}%", snap.cpu.usage_pct), p::brand()),
        kv(
            "load",
            format!(
                "{:.2} / {:.2} / {:.2}",
                snap.cpu.load_1, snap.cpu.load_5, snap.cpu.load_15
            ),
            p::text_primary(),
        ),
        kv("cores", format!("{}", snap.cpu.per_core.len()), p::text_primary()),
        kv("model", snap.host.cpu_model.clone(), p::text_muted()),
    ];
    f.render_widget(
        Paragraph::new(counters).style(Style::default().bg(p::bg())),
        cols[1],
    );
}

fn draw_per_core(f: &mut Frame, area: Rect, snap: &Snapshot, style: GraphStyle) {
    let block = panel("Per-core utilization");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    for (i, pct) in snap.cpu.per_core.iter().enumerate() {
        let color = if *pct >= 85.0 {
            p::status_error()
        } else if *pct >= 60.0 {
            p::status_warn()
        } else {
            p::status_good()
        };
        let bar = block_bar_styled(*pct / 100.0, inner.width.saturating_sub(14), color, style);
        let mut spans = vec![Span::styled(
            format!("c{:>3} ", i),
            Style::default().fg(p::text_muted()),
        )];
        spans.extend(bar.spans);
        spans.push(Span::styled(
            format!(" {:>5.1}%", pct),
            Style::default().fg(p::text_primary()).add_modifier(Modifier::BOLD),
        ));
        lines.push(Line::from(spans));
    }
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(p::bg())),
        inner,
    );
}

fn kv(k: &str, v: String, val_color: ratatui::style::Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{:<8}", k), Style::default().fg(p::text_muted())),
        Span::styled(v, Style::default().fg(val_color)),
    ])
}
