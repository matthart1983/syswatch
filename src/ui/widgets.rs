use ratatui::{
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders},
};

use crate::ui::graph::GraphStyle;
use crate::ui::palette as p;

/// Standard panel block: faint borders, dim title, BG fill.
///
/// Title is consumed (owned String) so callers can pass `format!(...)` directly
/// without the result being dropped before the Block is rendered.
pub fn panel(title: impl Into<String>) -> Block<'static> {
    let title: String = title.into();
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(p::border()))
        .title(Span::styled(
            format!(" {} ", title),
            Style::default().fg(p::text_muted()),
        ))
        .style(Style::default().bg(p::bg()))
}

/// Render a single horizontal block-bar of `width` cells filling `pct` (0..=1).
/// `Bars` style uses eighth-block characters (smooth solid fill);
/// `Dots` uses the btop-style braille progression for a textured look
/// that matches the line graph.
pub fn block_bar_styled(
    pct: f32,
    width: u16,
    color: ratatui::style::Color,
    style: GraphStyle,
) -> Line<'static> {
    let pct = pct.clamp(0.0, 1.0);
    let total_eighths = (width as f32 * 8.0 * pct).round() as u32;
    let full = (total_eighths / 8) as u16;
    let rem = (total_eighths % 8) as u8;

    // 9-state glyph tables: index 0 = empty (used for padding), 8 = full,
    // 1..=7 = partial fill levels matching `rem`.
    const BARS: [char; 9] = [
        ' ', '\u{258F}', // ▏ 1/8
        '\u{258E}', // ▎ 2/8
        '\u{258D}', // ▍ 3/8
        '\u{258C}', // ▌ 4/8
        '\u{258B}', // ▋ 5/8
        '\u{258A}', // ▊ 6/8
        '\u{2589}', // ▉ 7/8
        '\u{2588}', // █ full
    ];
    // btop braille progression: left column fills bottom-up, then right
    // column. 8 partial states + empty = 9.
    const DOTS: [char; 9] = [
        '\u{2800}', // ⠀ empty
        '\u{2840}', // ⡀ 1/8
        '\u{2844}', // ⡄ 2/8
        '\u{2846}', // ⡆ 3/8
        '\u{2847}', // ⡇ 4/8 (full left column)
        '\u{28C7}', // ⣇ 5/8
        '\u{28E7}', // ⣧ 6/8
        '\u{28F7}', // ⣷ 7/8
        '\u{28FF}', // ⣿ full
    ];
    let glyphs = match style {
        GraphStyle::Bars => &BARS,
        GraphStyle::Dots => &DOTS,
    };

    let mut s = String::new();
    for _ in 0..full {
        s.push(glyphs[8]);
    }
    if full < width && rem > 0 {
        s.push(glyphs[rem as usize]);
    }
    let pad = width.saturating_sub(s.chars().count() as u16);
    for _ in 0..pad {
        s.push(glyphs[0]);
    }
    Line::from(vec![Span::styled(s, Style::default().fg(color))])
}

/// Block sparkline glyphs `▁▂▃▄▅▆▇█` for the supplied normalized samples (0..=1).
pub fn sparkline(samples: &[f32], color: ratatui::style::Color) -> Line<'static> {
    const GLYPHS: [char; 8] = [
        '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}',
        '\u{2588}',
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
