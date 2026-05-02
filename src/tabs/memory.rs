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
    widgets::{block_bar, human_bytes, panel},
};

pub fn draw(f: &mut Frame, area: Rect, _app: &App, snap: &Snapshot) {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(7), Constraint::Length(7), Constraint::Min(0)])
        .split(area);

    draw_ram_bar(f, v[0], snap);
    draw_swap(f, v[1], snap);
    draw_top_rss(f, v[2], snap);
}

fn draw_ram_bar(f: &mut Frame, area: Rect, snap: &Snapshot) {
    let block = panel("RAM");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let total = snap.mem.total_bytes.max(1);
    let used = snap.mem.used_bytes;
    let avail = snap.mem.available_bytes;
    let pct = used as f32 / total as f32;
    let color = if pct >= 0.9 {
        p::RED
    } else if pct >= 0.7 {
        p::YELLOW
    } else {
        p::GREEN
    };

    let header = Line::from(vec![
        Span::styled("used ", Style::default().fg(p::DIM)),
        Span::styled(
            human_bytes(used),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" / ", Style::default().fg(p::DIM)),
        Span::styled(human_bytes(total), Style::default().fg(p::FG)),
        Span::styled(format!("   ({:>4.1}%)", pct * 100.0), Style::default().fg(p::DIM)),
        Span::styled("    available ", Style::default().fg(p::DIM)),
        Span::styled(human_bytes(avail), Style::default().fg(p::FG)),
    ]);
    let bar = block_bar(pct, inner.width, color);
    f.render_widget(
        Paragraph::new(vec![header, Line::from(""), bar]).style(Style::default().bg(p::BG)),
        inner,
    );
}

fn draw_swap(f: &mut Frame, area: Rect, snap: &Snapshot) {
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
        p::RED
    } else if pct >= 0.25 {
        p::YELLOW
    } else {
        p::GREEN
    };

    let header = if total == 0 {
        Line::from(vec![Span::styled(
            "no swap configured",
            Style::default().fg(p::DIM),
        )])
    } else {
        Line::from(vec![
            Span::styled("used ", Style::default().fg(p::DIM)),
            Span::styled(
                human_bytes(used),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" / ", Style::default().fg(p::DIM)),
            Span::styled(human_bytes(total), Style::default().fg(p::FG)),
            Span::styled(format!("   ({:>4.1}%)", pct * 100.0), Style::default().fg(p::DIM)),
        ])
    };
    let bar = block_bar(pct, inner.width, color);
    f.render_widget(
        Paragraph::new(vec![header, Line::from(""), bar]).style(Style::default().bg(p::BG)),
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
        Span::styled(format!("{:>7} ", "PID"), Style::default().fg(p::DIM).add_modifier(Modifier::BOLD)),
        Span::styled(format!("{:<10} ", "USER"), Style::default().fg(p::DIM).add_modifier(Modifier::BOLD)),
        Span::styled(format!("{:>10} ", "RSS"), Style::default().fg(p::DIM).add_modifier(Modifier::BOLD)),
        Span::styled(format!("{:>10} ", "VIRT"), Style::default().fg(p::DIM).add_modifier(Modifier::BOLD)),
        Span::styled("COMMAND", Style::default().fg(p::DIM).add_modifier(Modifier::BOLD)),
    ])];
    for proc_ in sorted.iter().take(take) {
        lines.push(Line::from(vec![
            Span::styled(format!("{:>7} ", proc_.pid), Style::default().fg(p::FG)),
            Span::styled(format!("{:<10.10} ", proc_.user), Style::default().fg(p::DIM)),
            Span::styled(
                format!("{:>10} ", human_bytes(proc_.mem_rss)),
                Style::default().fg(p::CYAN),
            ),
            Span::styled(format!("{:>10} ", human_bytes(proc_.mem_virt)), Style::default().fg(p::DIM)),
            Span::styled(proc_.name.clone(), Style::default().fg(p::FG)),
        ]));
    }
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(p::BG)),
        inner,
    );
}
