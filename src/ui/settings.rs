//! Settings popup. Shape mirrors netwatch's `ui::settings`: a centered
//! modal listing rows of label/value, ↑↓ to move, ←→ to cycle enum
//! values, Enter to edit numerics, S to save to disk, Esc to close.
//!
//! All mutation flows through `apply_edit` so the row→config mapping
//! lives in one place and stays unit-testable.

use ratatui::{
    layout::{Alignment, Rect},
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::app::App;
use crate::config::{SyswatchConfig, MAX_TICK_MS, MIN_TICK_MS};
use crate::ui::palette as p;
use crate::ui::theme;

/// Tab-name strings accepted by `default_tab`. Kept in sync with
/// `app::TabId::from_str_loose`. Order = cycle order in the popup.
pub const TAB_NAMES: &[&str] = &[
    "overview", "cpu", "memory", "disks", "fs", "procs", "gpu", "power", "services", "net",
    "timeline", "insights",
];

pub mod cursor {
    pub const THEME: usize = 0;
    pub const GRAPH_STYLE: usize = 1;
    pub const GRAPH_FADE: usize = 2;
    pub const DEFAULT_TAB: usize = 3;
    pub const TICK_MS: usize = 4;
}

pub const ROWS: usize = 5;

/// Whether the row at `idx` is enum-cycled (←→) vs free-text (Enter).
fn is_enum_row(idx: usize) -> bool {
    matches!(
        idx,
        cursor::THEME | cursor::GRAPH_STYLE | cursor::GRAPH_FADE | cursor::DEFAULT_TAB
    )
}

struct Row {
    label: &'static str,
    value: String,
}

fn build_rows(cfg: &SyswatchConfig) -> Vec<Row> {
    vec![
        Row {
            label: "Theme",
            value: cfg.theme.clone(),
        },
        Row {
            label: "Graph Style",
            value: cfg.graph_style.clone(),
        },
        Row {
            label: "Graph Fade (btop)",
            value: if cfg.graph_fade { "on" } else { "off" }.into(),
        },
        Row {
            label: "Default Tab",
            value: cfg.default_tab.clone(),
        },
        Row {
            label: "Tick (ms)",
            value: cfg.tick_ms.to_string(),
        },
    ]
}

pub fn render(f: &mut Frame, app: &App, area: Rect) {
    let popup_w = (area.width * 60 / 100)
        .max(50)
        .min(area.width.saturating_sub(4));
    let popup_h = (ROWS as u16 + 7).min(area.height.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(popup_w)) / 2;
    let y = area.y + (area.height.saturating_sub(popup_h)) / 2;
    let popup = Rect::new(x, y, popup_w, popup_h);

    f.render_widget(Clear, popup);

    let title = if let Some(path) = SyswatchConfig::path() {
        format!(" Settings — {} ", path.display())
    } else {
        " Settings ".to_string()
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(p::brand()))
        .style(Style::default().bg(p::bg()));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let rows = build_rows(&app.user_config);
    let label_w = 16;
    let mut lines: Vec<Line> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let selected = i == app.settings_cursor;
        let editing = selected && app.settings_editing;
        let indicator = if selected { "▸ " } else { "  " };
        let label_style = if selected {
            Style::default().fg(p::active_tab()).bold()
        } else {
            Style::default().fg(p::brand())
        };
        let value_display = if editing {
            format!("{}▏", app.settings_edit_buf)
        } else if selected && is_enum_row(i) {
            format!("◀ {} ▶", row.value)
        } else {
            row.value.clone()
        };
        let value_style = if editing {
            Style::default().fg(p::text_primary()).bg(p::selection_bg())
        } else if selected {
            Style::default().fg(p::text_primary())
        } else {
            Style::default().fg(p::text_muted())
        };
        lines.push(Line::from(vec![
            Span::styled(indicator.to_string(), label_style),
            Span::styled(
                format!("{:<width$}", row.label, width = label_w),
                label_style,
            ),
            Span::styled(value_display, value_style),
        ]));
    }
    lines.push(Line::raw(""));
    if let Some(status) = &app.settings_status {
        lines.push(Line::from(Span::styled(
            format!("  {}", status),
            Style::default().fg(p::status_good()),
        )));
    } else {
        lines.push(Line::raw(""));
    }

    let body_h = inner.height.saturating_sub(1);
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(p::bg())),
        Rect::new(inner.x, inner.y, inner.width, body_h),
    );

    // Footer hotkeys mirror the netwatch popup.
    let footer_spans: Vec<Span> = if app.settings_editing {
        vec![
            Span::styled("Enter", Style::default().fg(p::key_hint()).bold()),
            Span::raw(":Apply  "),
            Span::styled("Esc", Style::default().fg(p::key_hint()).bold()),
            Span::raw(":Cancel"),
        ]
    } else if is_enum_row(app.settings_cursor) {
        vec![
            Span::styled("←→", Style::default().fg(p::key_hint()).bold()),
            Span::raw(":Cycle  "),
            Span::styled("↑↓", Style::default().fg(p::key_hint()).bold()),
            Span::raw(":Navigate  "),
            Span::styled("S", Style::default().fg(p::key_hint()).bold()),
            Span::raw(":Save  "),
            Span::styled("Esc", Style::default().fg(p::key_hint()).bold()),
            Span::raw(":Close"),
        ]
    } else {
        vec![
            Span::styled("↑↓", Style::default().fg(p::key_hint()).bold()),
            Span::raw(":Navigate  "),
            Span::styled("Enter", Style::default().fg(p::key_hint()).bold()),
            Span::raw(":Edit  "),
            Span::styled("S", Style::default().fg(p::key_hint()).bold()),
            Span::raw(":Save  "),
            Span::styled("Esc", Style::default().fg(p::key_hint()).bold()),
            Span::raw(":Close"),
        ]
    };
    let footer_area = Rect::new(
        inner.x,
        inner.y + inner.height.saturating_sub(1),
        inner.width,
        1,
    );
    f.render_widget(
        Paragraph::new(Line::from(footer_spans))
            .alignment(Alignment::Center)
            .style(Style::default().bg(p::bg())),
        footer_area,
    );
}

/// Cycle to the next value for an enum row. No-op for non-enum rows.
pub fn cycle_next(cfg: &mut SyswatchConfig, cursor: usize) {
    match cursor {
        c if c == cursor::THEME => {
            let i = theme::THEME_NAMES
                .iter()
                .position(|n| **n == cfg.theme)
                .unwrap_or(0);
            cfg.theme = theme::THEME_NAMES[(i + 1) % theme::THEME_NAMES.len()].into();
        }
        c if c == cursor::GRAPH_STYLE => {
            cfg.graph_style = if cfg.graph_style == "bars" {
                "dots".into()
            } else {
                "bars".into()
            };
        }
        c if c == cursor::GRAPH_FADE => {
            cfg.graph_fade = !cfg.graph_fade;
        }
        c if c == cursor::DEFAULT_TAB => {
            let i = TAB_NAMES
                .iter()
                .position(|n| **n == cfg.default_tab)
                .unwrap_or(0);
            cfg.default_tab = TAB_NAMES[(i + 1) % TAB_NAMES.len()].into();
        }
        _ => {}
    }
}

pub fn cycle_prev(cfg: &mut SyswatchConfig, cursor: usize) {
    match cursor {
        c if c == cursor::THEME => {
            let i = theme::THEME_NAMES
                .iter()
                .position(|n| **n == cfg.theme)
                .unwrap_or(0);
            let prev = (i + theme::THEME_NAMES.len() - 1) % theme::THEME_NAMES.len();
            cfg.theme = theme::THEME_NAMES[prev].into();
        }
        c if c == cursor::GRAPH_STYLE => cycle_next(cfg, cursor), // 2-state, same direction
        c if c == cursor::GRAPH_FADE => cycle_next(cfg, cursor),  // toggle
        c if c == cursor::DEFAULT_TAB => {
            let i = TAB_NAMES
                .iter()
                .position(|n| **n == cfg.default_tab)
                .unwrap_or(0);
            let prev = (i + TAB_NAMES.len() - 1) % TAB_NAMES.len();
            cfg.default_tab = TAB_NAMES[prev].into();
        }
        _ => {}
    }
}

/// Apply a free-text edit (numerics) to the config. Returns Err with a
/// human-readable message on validation failure; caller surfaces it via
/// `App::settings_status`.
pub fn apply_edit(cfg: &mut SyswatchConfig, cursor: usize, value: &str) -> Result<(), String> {
    match cursor {
        c if c == cursor::TICK_MS => {
            let v: u64 = value
                .trim()
                .parse()
                .map_err(|_| "Tick must be a positive integer".to_string())?;
            if !(MIN_TICK_MS..=MAX_TICK_MS).contains(&v) {
                return Err(format!(
                    "Tick must be between {} and {} ms",
                    MIN_TICK_MS, MAX_TICK_MS
                ));
            }
            cfg.tick_ms = v;
            Ok(())
        }
        _ => Err("This row is enum-only — use ←/→ to cycle.".into()),
    }
}

/// Pre-fill the edit buffer with the current value at `cursor`.
pub fn edit_value(cfg: &SyswatchConfig, cursor: usize) -> String {
    match cursor {
        c if c == cursor::TICK_MS => cfg.tick_ms.to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> SyswatchConfig {
        SyswatchConfig::default()
    }

    #[test]
    fn cycle_theme_walks_full_list() {
        let mut c = cfg();
        let start = c.theme.clone();
        for _ in 0..theme::THEME_NAMES.len() {
            cycle_next(&mut c, cursor::THEME);
        }
        assert_eq!(c.theme, start, "full cycle should return to start");
    }

    #[test]
    fn cycle_graph_style_toggles() {
        let mut c = cfg();
        assert_eq!(c.graph_style, "bars");
        cycle_next(&mut c, cursor::GRAPH_STYLE);
        assert_eq!(c.graph_style, "dots");
        cycle_next(&mut c, cursor::GRAPH_STYLE);
        assert_eq!(c.graph_style, "bars");
    }

    #[test]
    fn cycle_prev_theme_walks_backwards() {
        let mut c = cfg();
        cycle_next(&mut c, cursor::THEME);
        let after_next = c.theme.clone();
        cycle_prev(&mut c, cursor::THEME);
        assert_eq!(c.theme, "dark", "prev from second theme returns to dark");
        cycle_next(&mut c, cursor::THEME);
        assert_eq!(c.theme, after_next);
    }

    #[test]
    fn cycle_default_tab_visits_every_tab() {
        let mut c = cfg();
        let mut seen = Vec::new();
        for _ in 0..TAB_NAMES.len() {
            cycle_next(&mut c, cursor::DEFAULT_TAB);
            seen.push(c.default_tab.clone());
        }
        let mut sorted = seen.clone();
        sorted.sort();
        let mut expected: Vec<String> = TAB_NAMES.iter().map(|s| s.to_string()).collect();
        expected.sort();
        assert_eq!(sorted, expected);
    }

    #[test]
    fn apply_edit_tick_accepts_in_range() {
        let mut c = cfg();
        assert!(apply_edit(&mut c, cursor::TICK_MS, "500").is_ok());
        assert_eq!(c.tick_ms, 500);
    }

    #[test]
    fn apply_edit_tick_rejects_out_of_range() {
        let mut c = cfg();
        assert!(apply_edit(&mut c, cursor::TICK_MS, "50").is_err());
        assert!(apply_edit(&mut c, cursor::TICK_MS, "10000").is_err());
        assert!(apply_edit(&mut c, cursor::TICK_MS, "abc").is_err());
        // None of the failed edits should have mutated the value.
        assert_eq!(c.tick_ms, 1000);
    }

    #[test]
    fn apply_edit_enum_row_returns_helpful_error() {
        let mut c = cfg();
        let err = apply_edit(&mut c, cursor::THEME, "dracula").unwrap_err();
        assert!(err.contains("←") || err.contains("cycle"));
    }

    #[test]
    fn edit_value_returns_current_tick() {
        let c = SyswatchConfig {
            tick_ms: 750,
            ..Default::default()
        };
        assert_eq!(edit_value(&c, cursor::TICK_MS), "750");
    }
}
