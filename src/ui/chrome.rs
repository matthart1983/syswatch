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

/// Render the configured sample interval compactly: `1s`, `1.5s`, `250ms`.
fn fmt_rate(tick_ms: u64) -> String {
    if tick_ms >= 1000 {
        if tick_ms.is_multiple_of(1000) {
            format!("{}s", tick_ms / 1000)
        } else {
            format!("{:.1}s", tick_ms as f32 / 1000.0)
        }
    } else {
        format!("{}ms", tick_ms)
    }
}

pub fn draw_header(
    f: &mut Frame,
    area: Rect,
    snap: &Snapshot,
    live: LiveState,
    recording: bool,
    tick_ms: u64,
) {
    let mut spans: Vec<Span> = Vec::new();
    spans.push(Span::styled(
        " \u{25cf}",
        Style::default().fg(p::status_good()),
    ));
    spans.push(Span::styled(
        " SysWatch",
        Style::default().fg(p::brand()).add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(
        format!(" v{}", env!("CARGO_PKG_VERSION")),
        Style::default().fg(p::text_muted()),
    ));
    spans.push(Span::styled(
        "  \u{2502}  ",
        Style::default().fg(p::border()),
    ));
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
    spans.push(Span::styled(
        "  load ",
        Style::default().fg(p::text_muted()),
    ));
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
        LiveState::Replay => ("REPLAY", p::tx_rate()),
    };
    let ts: chrono::DateTime<chrono::Local> = snap.t.into();
    // Right side is built as multiple spans so REC (when active) can
    // stand out in red against the live-state badge color.
    let mut right_spans: Vec<Span> = Vec::new();
    if recording {
        right_spans.push(Span::styled(
            "\u{23FA} REC  ",
            Style::default()
                .fg(p::status_error())
                .add_modifier(Modifier::BOLD),
        ));
    }
    // Refresh rate sits between the live badge and the clock so the user can
    // always see how fast the dashboard is sampling (issue #14).
    right_spans.push(Span::styled(
        format!("\u{25cf} {}  ", label),
        Style::default()
            .fg(right_color)
            .add_modifier(Modifier::BOLD),
    ));
    right_spans.push(Span::styled(
        format!("\u{27f3} {}  ", fmt_rate(tick_ms)),
        Style::default().fg(p::text_muted()),
    ));
    right_spans.push(Span::styled(
        ts.format("%H:%M:%S").to_string(),
        Style::default()
            .fg(right_color)
            .add_modifier(Modifier::BOLD),
    ));
    let right_text_w: usize = right_spans.iter().map(|s| s.content.chars().count()).sum();

    // Two paragraphs: left fills, right is a separate one-row area on the right edge.
    let right_w = right_text_w as u16 + 1;
    let left_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width.saturating_sub(right_w),
        height: 1,
    };
    let right_area = Rect {
        x: area.x + area.width.saturating_sub(right_w),
        y: area.y,
        // Clamp to the buffer: on very narrow terminals right_w can exceed
        // area.width, which would otherwise index past the buffer edge.
        width: right_w.min(area.width),
        height: 1,
    };

    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(p::bg()).fg(p::text_primary())),
        left_area,
    );
    f.render_widget(
        Paragraph::new(Line::from(right_spans)).style(Style::default().bg(p::bg())),
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
                    Style::default()
                        .fg(p::status_warn())
                        .add_modifier(Modifier::BOLD),
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

pub fn draw_footer(
    f: &mut Frame,
    area: Rect,
    graph_style: GraphStyle,
    flash: Option<&str>,
    show_filter: bool,
) {
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
        Paragraph::new(Line::from(Span::styled(
            sep,
            Style::default().fg(p::border()),
        )))
        .style(Style::default().bg(p::bg())),
        sep_area,
    );

    let graph_label: String = format!("Graph[{}]", graph_style.label());
    let theme_label: String = format!("Theme[{}]", crate::ui::theme::name());
    // Footer advertises only the keys that actually do something today.
    // Diff (D) is Phase 2 work; Profile (P) was an early aspiration
    // that's now an explicit non-goal — see plan.md.
    //
    // Filter is tab-scoped, so the hint is too: advertising it on the
    // nine tabs with nothing to search is what made issue #20 read as a
    // broken keybinding rather than a missing feature.
    let nav: &[(&str, &str)] = if show_filter {
        &[("/f", "Filter"), ("q", "Quit"), ("1-9", "Tab")]
    } else {
        &[("q", "Quit"), ("1-9", "Tab")]
    };
    let groups: &[&[(&str, &str)]] = &[
        &[("p", "Pause"), (",", "Settings")],
        &[("S", "Snapshot"), ("R", "Record")],
        &[("g", graph_label.as_str()), ("t", theme_label.as_str())],
        nav,
        &[("?", "Help")],
    ];
    // Transient flash takes the whole footer when active — used by the
    // S snapshot key to confirm the dump path. Cleaner than wedging it
    // between hotkey groups.
    if let Some(msg) = flash {
        let line = Line::from(vec![
            Span::raw(" "),
            Span::styled(
                msg.to_string(),
                Style::default()
                    .fg(p::status_good())
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        f.render_widget(
            Paragraph::new(line).style(Style::default().bg(p::bg())),
            hot_area,
        );
        return;
    }

    let mut spans: Vec<Span> = vec![Span::raw(" ")];
    for (gi, group) in groups.iter().enumerate() {
        if gi > 0 {
            spans.push(Span::styled(" \u{2502} ", Style::default().fg(p::border())));
        }
        for (k, label) in *group {
            spans.push(Span::styled(
                k.to_string(),
                Style::default()
                    .fg(p::key_hint())
                    .add_modifier(Modifier::BOLD),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_uptime_under_day_uses_hms() {
        assert_eq!(format_uptime(0), "00:00:00");
        assert_eq!(format_uptime(59), "00:00:59");
        assert_eq!(format_uptime(60), "00:01:00");
        assert_eq!(format_uptime(3661), "01:01:01");
    }

    #[test]
    fn format_uptime_last_second_before_day_rollover() {
        // 86399 = 23:59:59 — final second before the day-format takes over.
        assert_eq!(format_uptime(86_399), "23:59:59");
    }

    #[test]
    fn format_uptime_one_day_drops_seconds() {
        // The day format intentionally hides seconds — at this scale the
        // header has too much else to fit and second-precision is noise.
        assert_eq!(format_uptime(86_400), "1d 00:00");
    }

    #[test]
    fn format_uptime_multi_day_composition() {
        // 3 days, 4 hours, 5 minutes (+ 6 seconds that should drop).
        let secs = 3 * 86_400 + 4 * 3600 + 5 * 60 + 6;
        assert_eq!(format_uptime(secs), "3d 04:05");
    }

    #[test]
    fn format_uptime_zero_pads_hms_components() {
        // Single-digit hours/minutes must zero-pad so column alignment
        // doesn't shift across ticks in the header.
        assert_eq!(format_uptime(3600), "01:00:00");
    }

    #[test]
    fn fmt_rate_formats_sub_second_and_whole_and_fractional() {
        assert_eq!(fmt_rate(250), "250ms");
        assert_eq!(fmt_rate(1000), "1s");
        assert_eq!(fmt_rate(2000), "2s");
        assert_eq!(fmt_rate(1500), "1.5s");
    }
}
