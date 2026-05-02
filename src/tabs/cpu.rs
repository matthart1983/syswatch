use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{App, Snapshot};
use crate::ui::{
    palette as p,
    widgets::{block_bar, panel, sparkline},
};

pub fn draw(f: &mut Frame, area: Rect, app: &App, snap: &Snapshot) {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(8), Constraint::Min(0)])
        .split(area);

    draw_aggregate(f, v[0], app, snap);
    draw_per_core(f, v[1], snap);
}

fn draw_aggregate(f: &mut Frame, area: Rect, app: &App, snap: &Snapshot) {
    let block = panel("Aggregate CPU (last ~120s)");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(inner);

    // Sparkline strip.
    let series = app.history.cpu.to_vec();
    let take = cols[0].width as usize;
    let slice = if series.len() > take {
        series[series.len() - take..].to_vec()
    } else {
        series
    };
    let normalized: Vec<f32> = slice.iter().map(|v| (v / 100.0).min(1.0)).collect();

    let spark_area = Rect {
        x: cols[0].x,
        y: cols[0].y + 1,
        width: cols[0].width,
        height: cols[0].height.saturating_sub(2),
    };
    let lines: Vec<Line> = (0..spark_area.height).map(|_| sparkline(&normalized, p::CYAN)).collect();
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(p::BG)),
        spark_area,
    );

    // Counters panel.
    let counters = vec![
        kv("usage", format!("{:>5.1}%", snap.cpu.usage_pct), p::CYAN),
        kv(
            "load",
            format!(
                "{:.2} / {:.2} / {:.2}",
                snap.cpu.load_1, snap.cpu.load_5, snap.cpu.load_15
            ),
            p::FG,
        ),
        kv("cores", format!("{}", snap.cpu.per_core.len()), p::FG),
        kv("model", snap.host.cpu_model.clone(), p::DIM),
    ];
    f.render_widget(
        Paragraph::new(counters).style(Style::default().bg(p::BG)),
        cols[1],
    );
}

fn draw_per_core(f: &mut Frame, area: Rect, snap: &Snapshot) {
    let block = panel("Per-core utilization");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    for (i, pct) in snap.cpu.per_core.iter().enumerate() {
        let color = if *pct >= 85.0 {
            p::RED
        } else if *pct >= 60.0 {
            p::YELLOW
        } else {
            p::GREEN
        };
        let bar = block_bar(*pct / 100.0, inner.width.saturating_sub(14), color);
        let mut spans = vec![Span::styled(
            format!("c{:>3} ", i),
            Style::default().fg(p::DIM),
        )];
        spans.extend(bar.spans);
        spans.push(Span::styled(
            format!(" {:>5.1}%", pct),
            Style::default()
                .fg(p::FG)
                .add_modifier(Modifier::BOLD),
        ));
        lines.push(Line::from(spans));
    }
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(p::BG)),
        inner,
    );
}

fn kv(k: &str, v: String, val_color: ratatui::style::Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{:<8}", k), Style::default().fg(p::DIM)),
        Span::styled(v, Style::default().fg(val_color)),
    ])
}
