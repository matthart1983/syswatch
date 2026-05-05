use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{LiveState, Snapshot, TabId, ALL_TABS};
use crate::ui::graph::GraphStyle;
use crate::ui::palette as p;

pub fn draw_header(f: &mut Frame, area: Rect, snap: &Snapshot, live: LiveState) {
    let mut spans: Vec<Span> = Vec::new();
    spans.push(Span::styled(" \u{25cf}", Style::default().fg(p::status_good())));
    spans.push(Span::styled(
        " SysWatch",
        Style::default().fg(p::brand()).add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(
        format!(" v{}", env!("CARGO_PKG_VERSION")),
        Style::default().fg(p::text_muted()),
    ));
    spans.push(Span::styled("  \u{2502}  ", Style::default().fg(p::border())));
    spans.push(Span::styled("host ", Style::default().fg(p::text_muted())));
    spans.push(Span::styled(
        snap.host.hostname.clone(),
        Style::default().fg(p::text_primary()),
    ));
    spans.push(Span::styled("  ", Style::default().fg(p::text_muted())));
    spans.push(Span::styled(
        snap.host.os.clone(),
        Style::default().fg(p::text_primary()),
    ));
    spans.push(Span::styled("  up ", Style::default().fg(p::text_muted())));
    spans.push(Span::styled(
        format_uptime(snap.host.uptime_secs),
        Style::default().fg(p::text_primary()),
    ));
    spans.push(Span::styled("  load ", Style::default().fg(p::text_muted())));
    spans.push(Span::styled(
        format!(
            "{:.2} {:.2} {:.2}",
            snap.cpu.load_1, snap.cpu.load_5, snap.cpu.load_15
        ),
        Style::default().fg(p::text_primary()),
    ));

    let (label, right_color) = match live {
        LiveState::Live => ("LIVE", p::status_good()),
        LiveState::Paused => ("PAUSE", p::status_warn()),
        LiveState::Scrub => ("SCRUB", p::status_info()),
    };
    let ts: chrono::DateTime<chrono::Local> = snap.t.into();
    let right = format!("\u{25cf} {}  {}", label, ts.format("%H:%M:%S"));

    // Two paragraphs: left fills, right is a separate one-row area on the right edge.
    let right_w = right.chars().count() as u16 + 1;
    let left_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width.saturating_sub(right_w),
        height: 1,
    };
    let right_area = Rect {
        x: area.x + area.width.saturating_sub(right_w),
        y: area.y,
        width: right_w,
        height: 1,
    };

    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(p::bg()).fg(p::text_primary())),
        left_area,
    );
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            right,
            Style::default()
                .fg(right_color)
                .add_modifier(Modifier::BOLD),
        )]))
        .style(Style::default().bg(p::bg())),
        right_area,
    );
}

pub fn draw_tab_bar(f: &mut Frame, area: Rect, active: TabId, insight_count: usize) {
    // Row 0: tab labels. Row 1: thin underline with corner glyphs around active.
    let mut label_spans: Vec<Span> = Vec::new();
    let mut underline = String::new();
    let mut active_start: Option<usize> = None;
    let mut active_end: Option<usize> = None;
    let mut col: usize = 0;

    for tab in ALL_TABS {
        let badge_suffix = if *tab == TabId::Insights && insight_count > 0 {
            format!(" {}", insight_count)
        } else {
            String::new()
        };
        let label = format!(" [{}] {}{} ", tab.glyph(), tab.title(), badge_suffix);
        let w = label.chars().count();
        if *tab == active {
            label_spans.push(Span::styled(
                label.clone(),
                Style::default()
                    .fg(p::active_tab())
                    .bg(p::bg())
                    .add_modifier(Modifier::BOLD),
            ));
            active_start = Some(col);
            active_end = Some(col + w);
            for _ in 0..w {
                underline.push(' ');
            }
        } else {
            label_spans.push(Span::styled(
                format!(" [{}] ", tab.glyph()),
                Style::default().fg(p::text_muted()),
            ));
            label_spans.push(Span::styled(
                tab.title().to_string(),
                Style::default().fg(p::inactive_tab()),
            ));
            if !badge_suffix.is_empty() {
                label_spans.push(Span::styled(
                    badge_suffix,
                    Style::default().fg(p::status_warn()).add_modifier(Modifier::BOLD),
                ));
            }
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

    let label_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: 1,
    };
    let underline_area = Rect {
        x: area.x,
        y: area.y + 1,
        width: area.width,
        height: 1,
    };
    f.render_widget(
        Paragraph::new(Line::from(label_spans)).style(Style::default().bg(p::bg())),
        label_area,
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            underline,
            Style::default().fg(p::border()),
        )))
        .style(Style::default().bg(p::bg())),
        underline_area,
    );
}

pub fn draw_footer(f: &mut Frame, area: Rect, graph_style: GraphStyle) {
    // Row 0: thin separator. Row 1: hotkey strip.
    let sep_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: 1,
    };
    let hot_area = Rect {
        x: area.x,
        y: area.y + 1,
        width: area.width,
        height: 1,
    };
    let sep: String = std::iter::repeat('\u{2500}')
        .take(area.width as usize)
        .collect();
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(sep, Style::default().fg(p::border()))))
            .style(Style::default().bg(p::bg())),
        sep_area,
    );

    let graph_label: String = format!("Graph[{}]", graph_style.label());
    let theme_label: String = format!("Theme[{}]", crate::ui::theme::name());
    let groups: &[&[(&str, &str)]] = &[
        &[("p", "Pause"), (",", "Settings")],
        &[
            ("S", "Snapshot"),
            ("D", "Diff"),
            ("P", "Profile"),
            ("R", "Rec"),
        ],
        &[("g", graph_label.as_str()), ("t", theme_label.as_str())],
        &[("/", "Filter"), ("q", "Quit"), ("1-9", "Tab")],
        &[("?", "Help")],
    ];
    let mut spans: Vec<Span> = vec![Span::raw(" ")];
    for (gi, group) in groups.iter().enumerate() {
        if gi > 0 {
            spans.push(Span::styled(" \u{2502} ", Style::default().fg(p::border())));
        }
        for (k, label) in *group {
            spans.push(Span::styled(
                k.to_string(),
                Style::default().fg(p::key_hint()).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                format!(":{} ", label),
                Style::default().fg(p::text_muted()),
            ));
        }
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(p::bg())),
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
