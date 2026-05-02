use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{Snapshot, TabId, ALL_TABS};
use crate::ui::palette as p;

pub fn draw_header(f: &mut Frame, area: Rect, snap: &Snapshot) {
    let mut spans: Vec<Span> = Vec::new();
    spans.push(Span::styled(" \u{25cf}", Style::default().fg(p::GREEN)));
    spans.push(Span::styled(
        " SysWatch",
        Style::default().fg(p::CYAN).add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(
        format!(" v{}", env!("CARGO_PKG_VERSION")),
        Style::default().fg(p::DIM),
    ));
    spans.push(Span::styled("  \u{2502}  ", Style::default().fg(p::FAINT)));
    spans.push(Span::styled("host ", Style::default().fg(p::DIM)));
    spans.push(Span::styled(snap.host.hostname.clone(), Style::default().fg(p::FG)));
    spans.push(Span::styled("  ", Style::default().fg(p::DIM)));
    spans.push(Span::styled(snap.host.os.clone(), Style::default().fg(p::FG)));
    spans.push(Span::styled("  up ", Style::default().fg(p::DIM)));
    spans.push(Span::styled(format_uptime(snap.host.uptime_secs), Style::default().fg(p::FG)));
    spans.push(Span::styled("  load ", Style::default().fg(p::DIM)));
    spans.push(Span::styled(
        format!(
            "{:.2} {:.2} {:.2}",
            snap.cpu.load_1, snap.cpu.load_5, snap.cpu.load_15
        ),
        Style::default().fg(p::FG),
    ));

    let now = chrono::Local::now().format("%H:%M:%S").to_string();
    let right = format!("\u{25cf} {}  {}", if snap.live { "LIVE" } else { "PAUSE" }, now);
    let right_color = if snap.live { p::GREEN } else { p::YELLOW };

    // Two paragraphs: left fills, right is a separate one-row area on the right edge.
    let right_w = right.chars().count() as u16 + 1;
    let left_area = Rect { x: area.x, y: area.y, width: area.width.saturating_sub(right_w), height: 1 };
    let right_area = Rect { x: area.x + area.width.saturating_sub(right_w), y: area.y, width: right_w, height: 1 };

    f.render_widget(Paragraph::new(Line::from(spans)).style(Style::default().bg(p::BG).fg(p::FG)), left_area);
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            right,
            Style::default().fg(right_color).add_modifier(Modifier::BOLD),
        )]))
        .style(Style::default().bg(p::BG)),
        right_area,
    );
}

pub fn draw_tab_bar(f: &mut Frame, area: Rect, active: TabId) {
    // Row 0: tab labels. Row 1: thin underline with corner glyphs around active.
    let mut label_spans: Vec<Span> = Vec::new();
    let mut underline = String::new();
    let mut active_start: Option<usize> = None;
    let mut active_end: Option<usize> = None;
    let mut col: usize = 0;

    for tab in ALL_TABS {
        let label = format!(" [{}] {} ", tab.glyph(), tab.title());
        let w = label.chars().count();
        if *tab == active {
            label_spans.push(Span::styled(
                label.clone(),
                Style::default()
                    .fg(p::CYAN)
                    .bg(p::BG)
                    .add_modifier(Modifier::BOLD),
            ));
            active_start = Some(col);
            active_end = Some(col + w);
            for _ in 0..w {
                underline.push(' ');
            }
        } else {
            label_spans.push(Span::styled(format!(" [{}] ", tab.glyph()), Style::default().fg(p::DIM)));
            label_spans.push(Span::styled(tab.title().to_string(), Style::default().fg(p::FG)));
            label_spans.push(Span::raw(" "));
            for _ in 0..w {
                underline.push('\u{2500}');
            }
        }
        col += w;
    }
    // Pad underline to area.width.
    while (underline.chars().count() as u16) < area.width {
        underline.push('\u{2500}');
    }
    // Insert corner glyphs at active boundaries.
    if let (Some(s), Some(e)) = (active_start, active_end) {
        let mut chars: Vec<char> = underline.chars().collect();
        if s < chars.len() {
            chars[s] = '\u{2518}'; // ┘
        }
        if e > 0 && e - 1 < chars.len() {
            chars[e - 1] = '\u{2514}'; // └
        }
        underline = chars.into_iter().collect();
    }

    let label_area = Rect { x: area.x, y: area.y, width: area.width, height: 1 };
    let underline_area = Rect { x: area.x, y: area.y + 1, width: area.width, height: 1 };
    f.render_widget(
        Paragraph::new(Line::from(label_spans)).style(Style::default().bg(p::BG)),
        label_area,
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(underline, Style::default().fg(p::FAINT))))
            .style(Style::default().bg(p::BG)),
        underline_area,
    );
}

pub fn draw_footer(f: &mut Frame, area: Rect) {
    // Row 0: thin separator. Row 1: hotkey strip.
    let sep_area = Rect { x: area.x, y: area.y, width: area.width, height: 1 };
    let hot_area = Rect { x: area.x, y: area.y + 1, width: area.width, height: 1 };
    let sep: String = std::iter::repeat('\u{2500}').take(area.width as usize).collect();
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(sep, Style::default().fg(p::FAINT))))
            .style(Style::default().bg(p::BG)),
        sep_area,
    );

    let groups: &[&[(&str, &str)]] = &[
        &[("p", "Pause"), (",", "Settings")],
        &[("S", "Snapshot"), ("D", "Diff"), ("P", "Profile"), ("R", "Rec")],
        &[("/", "Filter"), ("q", "Quit"), ("1-9", "Tab")],
        &[("?", "Help")],
    ];
    let mut spans: Vec<Span> = vec![Span::raw(" ")];
    for (gi, group) in groups.iter().enumerate() {
        if gi > 0 {
            spans.push(Span::styled(" \u{2502} ", Style::default().fg(p::FAINT)));
        }
        for (k, label) in *group {
            spans.push(Span::styled(
                k.to_string(),
                Style::default().fg(p::CYAN).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(format!(":{} ", label), Style::default().fg(p::DIM)));
        }
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(p::BG)),
        hot_area,
    );
}

fn format_uptime(secs: u64) -> String {
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if d > 0 {
        format!("{}d {:02}:{:02}", d, h, m)
    } else {
        format!("{:02}:{:02}:{:02}", h, m, s)
    }
}
