use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{App, Snapshot};
use crate::insights::{Insight, Severity};
use crate::ui::palette as p;

pub fn draw(f: &mut Frame, area: Rect, app: &App, _snap: &Snapshot) {
    if area.height < 4 || area.width < 20 {
        return;
    }

    // Top status strip.
    let strip_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: 1,
    };
    draw_strip(f, strip_area, &app.insights);

    // Cards area starts one row below the strip.
    let cards_area = Rect {
        x: area.x,
        y: area.y + 1,
        width: area.width,
        height: area.height.saturating_sub(2),
    };
    draw_cards(f, cards_area, &app.insights);

    let footer_area = Rect {
        x: area.x,
        y: area.y + area.height - 1,
        width: area.width,
        height: 1,
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            "  Insights are read-only suggestions — they never modify processes, files, or services.",
            Style::default().fg(p::text_muted()),
        )]))
        .style(Style::default().bg(p::bg())),
        footer_area,
    );
}

fn draw_strip(f: &mut Frame, area: Rect, insights: &[Insight]) {
    let crit = insights
        .iter()
        .filter(|i| i.severity == Severity::Crit)
        .count();
    let warn = insights
        .iter()
        .filter(|i| i.severity == Severity::Warn)
        .count();
    let info = insights
        .iter()
        .filter(|i| i.severity == Severity::Info)
        .count();
    let active = crit + warn;

    let dot_color = if crit > 0 {
        p::status_error()
    } else if warn > 0 {
        p::status_warn()
    } else {
        p::status_good()
    };
    let summary = if active == 0 {
        Span::styled(
            "0 active  — system nominal",
            Style::default().fg(p::status_good()).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            format!("{} active", active),
            Style::default().fg(dot_color).add_modifier(Modifier::BOLD),
        )
    };
    let breakdown = format!("  ({} crit  {} warn  {} info)", crit, warn, info);
    let line = Line::from(vec![
        Span::styled(" \u{25cf} ", Style::default().fg(dot_color)),
        summary,
        Span::styled(breakdown, Style::default().fg(p::text_muted())),
    ]);
    f.render_widget(Paragraph::new(line).style(Style::default().bg(p::bg())), area);
}

fn draw_cards(f: &mut Frame, area: Rect, insights: &[Insight]) {
    if insights.is_empty() {
        draw_all_clear(f, area);
        return;
    }

    let card_h: u16 = 6;
    let max_cards = (area.height / card_h).max(1) as usize;
    let mut y = area.y;
    for ins in insights.iter().take(max_cards) {
        let card_rect = Rect {
            x: area.x,
            y,
            width: area.width,
            height: card_h.min(area.y + area.height - y),
        };
        draw_card(f, card_rect, ins);
        y += card_h;
    }
}

fn draw_card(f: &mut Frame, area: Rect, ins: &Insight) {
    let (sev_fg, sev_bg) = match ins.severity {
        Severity::Crit => (p::status_error(), p::err_bg()),
        Severity::Warn => (p::status_warn(), p::warn_bg()),
        Severity::Info => (p::brand(), p::selection_bg()),
    };

    // Render lines manually so we can paint the left stripe.
    // Row 0 (top border), 1 (badge + title), 2 (body1), 3 (body2), 4 (body3 / blank), 5 (suggested tab)
    let rows = area.height as usize;
    let w = area.width as usize;

    // Top border
    let top = Line::from(vec![Span::styled(
        format!("\u{250C}{}\u{2510}", "\u{2500}".repeat(w.saturating_sub(2))),
        Style::default().fg(p::border()),
    )]);
    // Bottom border (only rendered if there's room)
    let bot = Line::from(vec![Span::styled(
        format!("\u{2514}{}\u{2518}", "\u{2500}".repeat(w.saturating_sub(2))),
        Style::default().fg(p::border()),
    )]);

    let mut lines: Vec<Line> = Vec::with_capacity(rows);
    lines.push(top);

    // Row 1: stripe + badge + title
    let badge = format!(" {} ", ins.severity.label());
    let body_pad = "  ";
    let title_w = w
        .saturating_sub(1) // stripe
        .saturating_sub(badge.chars().count())
        .saturating_sub(body_pad.chars().count())
        .saturating_sub(1); // right border
    let title_truncated = truncate(&ins.title, title_w);
    lines.push(Line::from(vec![
        Span::styled("\u{2503}", Style::default().fg(sev_fg)), // ┃ stripe
        Span::styled(
            badge,
            Style::default()
                .fg(sev_fg)
                .bg(sev_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            title_truncated,
            Style::default().fg(p::text_primary()).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            pad_right(
                w,
                1 + ins.severity.label().chars().count()
                    + 2
                    + 1
                    + truncate(&ins.title, title_w).chars().count(),
                1,
            ),
            Style::default().bg(p::bg()),
        ),
        Span::styled("\u{2502}", Style::default().fg(p::border())),
    ]));

    // Body lines (up to 3)
    for i in 0..3 {
        if i + 2 >= rows.saturating_sub(1) {
            break;
        }
        let text = ins.body.get(i).cloned().unwrap_or_default();
        let body_color = if i == 0 { p::text_primary() } else { p::text_muted() };
        let truncated = truncate(&text, w.saturating_sub(4));
        lines.push(Line::from(vec![
            Span::styled("\u{2503}", Style::default().fg(sev_fg)),
            Span::raw("  "),
            Span::styled(truncated.clone(), Style::default().fg(body_color)),
            Span::styled(
                pad_right(w, 1 + 2 + truncated.chars().count(), 1),
                Style::default().bg(p::bg()),
            ),
            Span::styled("\u{2502}", Style::default().fg(p::border())),
        ]));
    }

    // Last interior row: suggested tab
    if rows >= 3 {
        let tab_label = format!("\u{2192} open {} tab", ins.suggested_tab.title());
        let truncated = truncate(&tab_label, w.saturating_sub(4));
        lines.push(Line::from(vec![
            Span::styled("\u{2503}", Style::default().fg(sev_fg)),
            Span::raw("  "),
            Span::styled(truncated.clone(), Style::default().fg(p::brand())),
            Span::styled(
                pad_right(w, 1 + 2 + truncated.chars().count(), 1),
                Style::default().bg(p::bg()),
            ),
            Span::styled("\u{2502}", Style::default().fg(p::border())),
        ]));
    }

    lines.push(bot);

    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(p::bg())),
        area,
    );
}

fn draw_all_clear(f: &mut Frame, area: Rect) {
    let card_rect = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: 6.min(area.height),
    };
    let ins = Insight {
        severity: Severity::Info,
        title: "no anomalies detected".into(),
        body: vec![
            "All checks passed: swap, runaway procs, disk fill, memory pressure, load, zombies."
                .into(),
            "Insights re-evaluate every tick from the rolling session window.".into(),
        ],
        suggested_tab: crate::app::TabId::Overview,
    };
    draw_card(f, card_rect, &ins);
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else if max <= 1 {
        s.chars().take(max).collect()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('\u{2026}'); // …
        out
    }
}

fn pad_right(width: usize, used: usize, right_reserve: usize) -> String {
    let target = width.saturating_sub(used).saturating_sub(right_reserve);
    " ".repeat(target)
}
