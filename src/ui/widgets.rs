use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders},
};

use crate::ui::palette as p;

/// Standard panel block: faint borders, dim title, BG fill.
pub fn panel(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(p::FAINT))
        .title(Span::styled(
            format!(" {} ", title),
            Style::default().fg(p::DIM),
        ))
        .style(Style::default().bg(p::BG))
}

/// Render a single horizontal block-bar of `width` cells filling `pct` (0..=1).
/// Uses the eighth-block characters for sub-cell precision.
pub fn block_bar(pct: f32, width: u16, color: ratatui::style::Color) -> Line<'static> {
    let pct = pct.clamp(0.0, 1.0);
    let total_eighths = (width as f32 * 8.0 * pct).round() as u32;
    let full = (total_eighths / 8) as u16;
    let rem = (total_eighths % 8) as u8;
    let mut s = String::new();
    for _ in 0..full {
        s.push('\u{2588}');
    }
    if full < width && rem > 0 {
        s.push(match rem {
            1 => '\u{258F}',
            2 => '\u{258E}',
            3 => '\u{258D}',
            4 => '\u{258C}',
            5 => '\u{258B}',
            6 => '\u{258A}',
            7 => '\u{2589}',
            _ => ' ',
        });
    }
    let pad = width.saturating_sub(s.chars().count() as u16);
    for _ in 0..pad {
        s.push(' ');
    }
    Line::from(vec![Span::styled(s, Style::default().fg(color))])
}

/// Block sparkline glyphs `▁▂▃▄▅▆▇█` for the supplied normalized samples (0..=1).
pub fn sparkline(samples: &[f32], color: ratatui::style::Color) -> Line<'static> {
    const GLYPHS: [char; 8] = [
        '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}', '\u{2588}',
    ];
    let s: String = samples
        .iter()
        .map(|v| {
            let v = v.clamp(0.0, 1.0);
            let idx = ((v * 7.0).round() as usize).min(7);
            GLYPHS[idx]
        })
        .collect();
    Line::from(vec![Span::styled(s, Style::default().fg(color))])
}

/// Status badge with bold text on a tinted background. Use for [OK]/[WARN]/[CRIT].
pub fn badge(text: &str, fg: ratatui::style::Color, bg: ratatui::style::Color) -> Span<'static> {
    Span::styled(
        format!(" {} ", text),
        Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
    )
}

pub fn human_bytes(b: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i + 1 < UNITS.len() {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", b, UNITS[i])
    } else {
        format!("{:.1} {}", v, UNITS[i])
    }
}

pub fn human_rate(b_per_s: f64) -> String {
    format!("{}/s", human_bytes(b_per_s as u64))
}
