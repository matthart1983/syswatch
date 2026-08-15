//! Help popup. `?` toggles a centered modal listing every hotkey
//! grouped by scope. Single source of truth for hotkey docs so the
//! footer hint strip and this dialog stay aligned.

use ratatui::{
    layout::{Alignment, Rect},
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::ui::palette as p;

/// One hotkey row inside a group.
struct Row {
    keys: &'static str,
    label: &'static str,
}

struct Group {
    title: &'static str,
    rows: &'static [Row],
}

/// All hotkeys, grouped by scope. Adding a new binding in App::handle_key
/// should add a row here too — the test below enforces non-emptiness as
/// a soft reminder, but the source-of-truth is this file.
const GROUPS: &[Group] = &[
    Group {
        title: "Global",
        rows: &[
            Row {
                keys: "1-9 0 - +",
                label: "Switch tab",
            },
            Row {
                keys: "Tab / Shift-Tab",
                label: "Next / previous tab",
            },
            Row {
                keys: "p",
                label: "Pause / resume sampling",
            },
            Row {
                keys: "g",
                label: "Cycle graph style (bars / dots)",
            },
            Row {
                keys: "t",
                label: "Cycle theme",
            },
            Row {
                keys: ",",
                label: "Open the settings menu (works in every view)",
            },
            Row {
                keys: "S",
                label: "Snapshot current sample → JSON file",
            },
            Row {
                keys: "R",
                label: "Toggle session recording → .swr file",
            },
            Row {
                keys: "V",
                label: "Cycle views: Full → Lite → Dense",
            },
            Row {
                keys: "L",
                label: "Jump straight to the Lite view (one screen, six keys)",
            },
            Row {
                keys: "?",
                label: "Toggle this help",
            },
            Row {
                keys: "q / Ctrl-C",
                label: "Quit",
            },
        ],
    },
    // Lite's footer advertises six keys and has room for no more. These two
    // are real bindings that live here instead — they are conventions from
    // less/vim/top, so they cost a user nothing to not see.
    Group {
        title: "Lite view (L)",
        rows: &[
            Row {
                keys: "↑ ↓ / j k",
                label: "Move the selection",
            },
            Row {
                keys: "Enter",
                label: "Expand the selected process in place",
            },
            Row {
                keys: "/",
                label: "Filter by process name or user",
            },
            Row {
                keys: "Esc",
                label: "Close detail, then clear the filter",
            },
            Row {
                keys: "L",
                label: "Back to the full tabbed view",
            },
        ],
    },
    // Dense advertises its keys in its own box borders, so this group only
    // lists the ones the borders have no room for.
    Group {
        title: "Dense view (V)",
        rows: &[
            Row {
                keys: "1 – 6",
                label: "Zoom that box to the full screen; same key or Esc to go back",
            },
            Row {
                keys: "↑ ↓ / j k",
                label: "Move the selection; the detail block follows it",
            },
            Row {
                keys: "PgUp / PgDn",
                label: "Move the selection a page at a time",
            },
            Row {
                keys: "Home / End",
                label: "First / last process",
            },
            Row {
                keys: "t",
                label: "Cycle the theme",
            },
        ],
    },
    Group {
        title: "Timeline / scrub",
        rows: &[
            Row {
                keys: "← / →",
                label: "Step one tick back / forward",
            },
            Row {
                keys: "Home",
                label: "Jump to oldest tick",
            },
            Row {
                keys: "End",
                label: "Return to live",
            },
        ],
    },
    Group {
        title: "Filter — Procs, Memory, Services",
        rows: &[
            Row {
                keys: "/ or f",
                label: "Filter the table (case-insensitive substring)",
            },
            Row {
                keys: "Enter",
                label: "Apply and leave the input box",
            },
            Row {
                keys: "Esc",
                label: "Cancel — clears the filter too",
            },
            Row {
                keys: "",
                label: "One filter, shared: it follows you between those tabs",
            },
        ],
    },
    Group {
        title: "Procs tab",
        rows: &[
            Row {
                keys: "↑ / ↓",
                label: "Move selection",
            },
            Row {
                keys: "s",
                label: "Cycle sort (cpu / mem / io / start / name / gpu / net)",
            },
            Row {
                keys: "d",
                label: "Toggle drill-in detail pane (more list rows)",
            },
        ],
    },
    Group {
        title: "Services tab",
        rows: &[
            Row {
                keys: "↑ / ↓",
                label: "Move selection",
            },
            Row {
                keys: "s",
                label: "Cycle sort (name / status / pid)",
            },
        ],
    },
    Group {
        title: "Settings popup",
        rows: &[
            Row {
                keys: "↑ / ↓",
                label: "Move cursor",
            },
            Row {
                keys: "← / →",
                label: "Cycle enum value",
            },
            Row {
                keys: "Enter",
                label: "Edit numeric value",
            },
            Row {
                keys: "S",
                label: "Save to disk",
            },
            Row {
                keys: "Esc",
                label: "Close",
            },
        ],
    },
];

pub fn render(f: &mut Frame, area: Rect) {
    // Compute desired height: title + blank + (group title + rows + blank) per group.
    let mut wanted_h: u16 = 4;
    for g in GROUPS {
        wanted_h = wanted_h.saturating_add(1 + g.rows.len() as u16 + 1);
    }
    let popup_w = (area.width * 70 / 100)
        .max(60)
        .min(area.width.saturating_sub(4));
    let popup_h = wanted_h.min(area.height.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(popup_w)) / 2;
    let y = area.y + (area.height.saturating_sub(popup_h)) / 2;
    let popup = Rect::new(x, y, popup_w, popup_h);

    f.render_widget(Clear, popup);
    let block = Block::default()
        .title(" Help ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(p::brand()))
        .style(Style::default().bg(p::bg()));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let mut lines: Vec<Line> = Vec::new();
    for group in GROUPS {
        lines.push(Line::from(Span::styled(
            format!(" {} ", group.title),
            Style::default().fg(p::active_tab()).bold(),
        )));
        for row in group.rows {
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(
                    format!("{:<18}", row.keys),
                    Style::default().fg(p::key_hint()).bold(),
                ),
                Span::styled(
                    row.label.to_string(),
                    Style::default().fg(p::text_primary()),
                ),
            ]));
        }
        lines.push(Line::raw(""));
    }

    // Footer line — single hint to close.
    let footer_h: u16 = 1;
    let body_h = inner.height.saturating_sub(footer_h);
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(p::bg())),
        Rect::new(inner.x, inner.y, inner.width, body_h),
    );
    let footer_area = Rect::new(inner.x, inner.y + body_h, inner.width, footer_h);
    let footer = Line::from(vec![
        Span::styled("Esc / ?", Style::default().fg(p::key_hint()).bold()),
        Span::raw(":Close"),
    ]);
    f.render_widget(
        Paragraph::new(footer)
            .alignment(Alignment::Center)
            .style(Style::default().bg(p::bg())),
        footer_area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_group_has_at_least_one_row() {
        for g in GROUPS {
            assert!(!g.rows.is_empty(), "group {:?} has no rows", g.title);
        }
    }

    #[test]
    fn at_least_the_global_essentials_are_documented() {
        // A regression check that future-me doesn't accidentally drop
        // the most-used keys when reorganizing.
        let global = GROUPS
            .iter()
            .find(|g| g.title == "Global")
            .expect("Global group present");
        let keys: String = global
            .rows
            .iter()
            .map(|r| r.keys)
            .collect::<Vec<_>>()
            .join("|");
        for needle in ["q", "p", "g", "t", "?", ","] {
            assert!(
                keys.contains(needle),
                "Global help missing essential key {:?}",
                needle
            );
        }
    }
}
