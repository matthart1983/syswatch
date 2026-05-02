use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::ui::{palette as p, widgets::panel};

pub fn draw(f: &mut Frame, area: Rect, title: &str, replaces: &str) {
    let block = panel(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let lines = vec![
        Line::from(vec![Span::styled(
            format!("{} tab — coming next", title),
            Style::default().fg(p::CYAN).add_modifier(Modifier::BOLD),
        )]),
        Line::from(""),
        Line::from(vec![
            Span::styled("Replaces: ", Style::default().fg(p::DIM)),
            Span::styled(replaces.to_string(), Style::default().fg(p::FG)),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled(
            "v0.1 ships Overview, CPU, Memory, Net.",
            Style::default().fg(p::DIM),
        )]),
        Line::from(vec![Span::styled(
            "Press 1-3 or 0 to try them, q to quit.",
            Style::default().fg(p::FAINT),
        )]),
    ];
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(p::BG)),
        inner,
    );
}
