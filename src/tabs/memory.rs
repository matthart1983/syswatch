use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{App, Snapshot};
use crate::ui::{
    graph::GraphStyle,
    palette as p,
    widgets::{block_bar_styled, human_bytes, panel},
};

pub fn draw(f: &mut Frame, area: Rect, app: &App, snap: &Snapshot) {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Length(7),
            Constraint::Min(0),
        ])
        .split(area);

    draw_ram_bar(f, v[0], snap, app.graph_style);
    draw_swap(f, v[1], snap, app.graph_style);
    draw_top_rss(f, v[2], snap);
}

fn draw_ram_bar(f: &mut Frame, area: Rect, snap: &Snapshot, style: GraphStyle) {
    let block = panel("RAM");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let total = snap.mem.total_bytes.max(1);
    let used = snap.mem.used_bytes;
    let avail = snap.mem.available_bytes;
    let pct = used as f32 / total as f32;
    let color = if pct >= 0.9 {
        p::status_error()
    } else if pct >= 0.7 {
        p::status_warn()
    } else {
        p::status_good()
    };

    let header = Line::from(vec![
        Span::styled("used ", Style::default().fg(p::text_muted())),
        Span::styled(
            human_bytes(used),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" / ", Style::default().fg(p::text_muted())),
        Span::styled(human_bytes(total), Style::default().fg(p::text_primary())),
        Span::styled(
            format!("   ({:>4.1}%)", pct * 100.0),
            Style::default().fg(p::text_muted()),
        ),
        Span::styled("    available ", Style::default().fg(p::text_muted())),
        Span::styled(human_bytes(avail), Style::default().fg(p::text_primary())),
    ]);
    let bar = block_bar_styled(pct, inner.width, color, style);
    f.render_widget(
        Paragraph::new(vec![header, Line::from(""), bar]).style(Style::default().bg(p::bg())),
        inner,
    );
}

fn draw_swap(f: &mut Frame, area: Rect, snap: &Snapshot, style: GraphStyle) {
    let block = panel("Swap");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let total = snap.mem.swap_total_bytes;
    let used = snap.mem.swap_used_bytes;
    let pct = if total > 0 {
        used as f32 / total as f32
    } else {
        0.0
    };
    let color = if pct >= 0.75 {
        p::status_error()
    } else if pct >= 0.25 {
        p::status_warn()
    } else {
        p::status_good()
    };

    let header = if total == 0 {
        Line::from(vec![Span::styled(
            "no swap configured",
            Style::default().fg(p::text_muted()),
        )])
    } else {
        Line::from(vec![
            Span::styled("used ", Style::default().fg(p::text_muted())),
            Span::styled(
                human_bytes(used),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" / ", Style::default().fg(p::text_muted())),
            Span::styled(human_bytes(total), Style::default().fg(p::text_primary())),
            Span::styled(
                format!("   ({:>4.1}%)", pct * 100.0),
                Style::default().fg(p::text_muted()),
            ),
        ])
    };
    let bar = block_bar_styled(pct, inner.width, color, style);
    f.render_widget(
        Paragraph::new(vec![header, Line::from(""), bar]).style(Style::default().bg(p::bg())),
        inner,
    );
}

fn draw_top_rss(f: &mut Frame, area: Rect, snap: &Snapshot) {
    let block = panel("Top processes (by RSS)");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut sorted = snap.procs.clone();
    sorted.sort_by(|a, b| b.mem_rss.cmp(&a.mem_rss));
    let take = inner.height.saturating_sub(1) as usize;

    let mut lines: Vec<Line> = vec![Line::from(vec![
        Span::styled(
            format!("{:>7} ", "PID"),
            Style::default().fg(p::text_muted()).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:<10} ", "USER"),
            Style::default().fg(p::text_muted()).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:>10} ", "RSS"),
            Style::default().fg(p::text_muted()).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:>10} ", "VIRT"),
            Style::default().fg(p::text_muted()).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "COMMAND",
            Style::default().fg(p::text_muted()).add_modifier(Modifier::BOLD),
        ),
    ])];
    for proc_ in sorted.iter().take(take) {
        lines.push(Line::from(vec![
            Span::styled(format!("{:>7} ", proc_.pid), Style::default().fg(p::text_primary())),
            Span::styled(
                format!("{:<10.10} ", proc_.user),
                Style::default().fg(p::text_muted()),
            ),
            Span::styled(
                format!("{:>10} ", human_bytes(proc_.mem_rss)),
                Style::default().fg(p::brand()),
            ),
            Span::styled(
                format!("{:>10} ", human_bytes(proc_.mem_virt)),
                Style::default().fg(p::text_muted()),
            ),
            Span::styled(proc_.name.clone(), Style::default().fg(p::text_primary())),
        ]));
    }
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(p::bg())),
        inner,
    );
}
