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
    widgets::{human_bytes, human_rate, panel, sparkline},
};

pub fn draw(f: &mut Frame, area: Rect, app: &App, snap: &Snapshot) {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(8), Constraint::Min(0)])
        .split(area);

    draw_aggregate(f, v[0], app, snap);
    draw_iface_table(f, v[1], snap);
}

fn draw_aggregate(f: &mut Frame, area: Rect, app: &App, snap: &Snapshot) {
    let block = panel("Aggregate throughput (rx + tx)");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let series: Vec<f32> = app
        .history
        .net_rate
        .to_vec()
        .iter()
        .map(|v| *v as f32)
        .collect();
    let peak = series.iter().cloned().fold(1.0f32, f32::max);
    let normalized: Vec<f32> = series.iter().map(|v| (v / peak).min(1.0)).collect();

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(inner);

    let spark_lines: Vec<Line> = (0..cols[0].height).map(|_| sparkline(&normalized, p::CYAN)).collect();
    f.render_widget(
        Paragraph::new(spark_lines).style(Style::default().bg(p::BG)),
        cols[0],
    );

    let total: f64 = snap.net.iter().map(|i| i.rx_rate + i.tx_rate).sum();
    let rx: f64 = snap.net.iter().map(|i| i.rx_rate).sum();
    let tx: f64 = snap.net.iter().map(|i| i.tx_rate).sum();
    let counters = vec![
        Line::from(vec![
            Span::styled("now   ", Style::default().fg(p::DIM)),
            Span::styled(
                human_rate(total),
                Style::default()
                    .fg(p::CYAN)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("rx    ", Style::default().fg(p::DIM)),
            Span::styled(human_rate(rx), Style::default().fg(p::GREEN)),
        ]),
        Line::from(vec![
            Span::styled("tx    ", Style::default().fg(p::DIM)),
            Span::styled(human_rate(tx), Style::default().fg(p::CYAN)),
        ]),
        Line::from(vec![
            Span::styled("peak  ", Style::default().fg(p::DIM)),
            Span::styled(human_rate(peak as f64), Style::default().fg(p::FG)),
        ]),
        Line::from(vec![
            Span::styled("ifaces ", Style::default().fg(p::DIM)),
            Span::styled(snap.net.len().to_string(), Style::default().fg(p::FG)),
        ]),
    ];
    f.render_widget(
        Paragraph::new(counters).style(Style::default().bg(p::BG)),
        cols[1],
    );
}

fn draw_iface_table(f: &mut Frame, area: Rect, snap: &Snapshot) {
    let block = panel("Interfaces (via netwatch-sdk)");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = vec![Line::from(vec![
        Span::styled(format!("{:<14} ", "NAME"), header_style()),
        Span::styled(format!("{:<6} ", "STATE"), header_style()),
        Span::styled(format!("{:>12} ", "RX/s"), header_style()),
        Span::styled(format!("{:>12} ", "TX/s"), header_style()),
        Span::styled(format!("{:>14} ", "RX TOTAL"), header_style()),
        Span::styled(format!("{:>14}", "TX TOTAL"), header_style()),
    ])];
    let take = inner.height.saturating_sub(1) as usize;
    for iface in snap.net.iter().take(take) {
        let state_color = if iface.is_up { p::GREEN } else { p::FAINT };
        lines.push(Line::from(vec![
            Span::styled(format!("{:<14.14} ", iface.name), Style::default().fg(p::FG)),
            Span::styled(
                format!("{:<6} ", if iface.is_up { "UP" } else { "DOWN" }),
                Style::default().fg(state_color),
            ),
            Span::styled(
                format!("{:>12} ", human_rate(iface.rx_rate)),
                Style::default().fg(p::GREEN),
            ),
            Span::styled(
                format!("{:>12} ", human_rate(iface.tx_rate)),
                Style::default().fg(p::CYAN),
            ),
            Span::styled(
                format!("{:>14} ", human_bytes(iface.rx_bytes)),
                Style::default().fg(p::DIM),
            ),
            Span::styled(
                format!("{:>14}", human_bytes(iface.tx_bytes)),
                Style::default().fg(p::DIM),
            ),
        ]));
    }
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(p::BG)),
        inner,
    );
}

fn header_style() -> Style {
    Style::default().fg(p::DIM).add_modifier(Modifier::BOLD)
}
