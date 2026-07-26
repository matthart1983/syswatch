use ratatui::{
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders},
};

use crate::ui::graph::GraphStyle;
use crate::ui::palette as p;

/// The one-line filter prompt, shared by every tab that supports `/`
/// (Procs, Memory, Services). Keeping it in one place is what makes the
/// interaction feel identical wherever it's triggered — issue #20 started
/// as "search doesn't work outside Procs".
pub fn filter_input_line(buf: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            " / ",
            Style::default()
                .fg(p::brand())
                .add_modifier(ratatui::style::Modifier::BOLD),
        ),
        Span::styled(buf.to_string(), Style::default().fg(p::text_primary())),
        Span::styled(
            "▏",
            Style::default()
                .fg(p::brand())
                .add_modifier(ratatui::style::Modifier::BOLD),
        ),
        Span::styled(
            "    Enter:apply  Esc:cancel",
            Style::default().fg(p::text_muted()),
        ),
    ])
}

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

/// Style-aware sparkline that produces a `Line` (so callers like the
/// Timeline tab can composite a label + cursor marker around it).
/// `GraphStyle::Bars` renders the original block glyphs; `GraphStyle::Dots`
/// renders a 5-level braille fill from the bottom of the cell — the
/// 1-cell-tall analogue of `graph::render_dots`. Used by Timeline strips
/// so the `g` toggle that flips every multi-row chart also flips inline
/// sparklines.
pub fn sparkline_styled(
    samples: &[f32],
    color: ratatui::style::Color,
    style: GraphStyle,
) -> Line<'static> {
    Line::from(vec![Span::styled(
        sparkline_string(samples, style),
        Style::default().fg(color),
    )])
}

/// Glyph string for `samples` (each 0..1), one cell per sample. Callers that
/// need to composite the sparkline into a wider styled row (e.g. the Procs
/// per-process CPU trend column) use this directly.
pub fn sparkline_string(samples: &[f32], style: GraphStyle) -> String {
    samples.iter().map(|v| sparkline_glyph(*v, style)).collect()
}

fn sparkline_glyph(v: f32, style: GraphStyle) -> char {
    let v = v.clamp(0.0, 1.0);
    match style {
        GraphStyle::Bars => {
            const GLYPHS: [char; 8] = [
                '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}',
                '\u{2588}',
            ];
            let idx = ((v * 7.0).round() as usize).min(7);
            GLYPHS[idx]
        }
        GraphStyle::Dots => {
            // 5 levels of braille fill in a single cell. Each level adds
            // the next pixel row from the bottom up across both
            // sub-columns. Matches `graph::render_dots` visually at
            // cell_h=1: the strip reads as a continuous filled area
            // composed of stippled braille rather than solid blocks.
            const DOT_LEVELS: [char; 5] = [
                ' ',        // 0/4 — empty
                '\u{28C0}', // 1/4 — bottom row (dots 7,8)
                '\u{28E4}', // 2/4 — bottom + row above (dots 7,8,3,6)
                '\u{28F6}', // 3/4 — three rows from bottom (dots 7,8,3,6,2,5)
                '\u{28FF}', // 4/4 — full cell (all 8 dots)
            ];
            let level = (v * 4.0).round() as usize;
            DOT_LEVELS[level.min(4)]
        }
    }
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

/// Memory utilisation as percent of total RAM — the %MEM the process
/// tables render. Total is clamped to 1 so a zeroed snapshot (first
/// tick, malformed recording) renders 0.0 rather than NaN.
pub fn mem_pct(bytes: u64, total_bytes: u64) -> f32 {
    bytes as f32 / total_bytes.max(1) as f32 * 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_bytes_zero() {
        assert_eq!(human_bytes(0), "0 B");
    }

    #[test]
    fn human_bytes_under_one_kib_renders_as_bytes_no_decimal() {
        // The < 1024 branch uses the integer count, not the float — so
        // no ".0" sneaks in for small byte counts.
        assert_eq!(human_bytes(1), "1 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1023), "1023 B");
    }

    #[test]
    fn human_bytes_one_kib_rolls_to_kb_unit_with_decimal() {
        // Roll-over from B → KB is the most common formatting bug.
        assert_eq!(human_bytes(1024), "1.0 KB");
    }

    #[test]
    fn human_bytes_progresses_through_units() {
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(human_bytes(2 * 1024 * 1024 * 1024), "2.0 GB");
        assert_eq!(human_bytes(3u64 * 1024 * 1024 * 1024 * 1024), "3.0 TB");
    }

    #[test]
    fn human_bytes_caps_at_pb_for_max_u64() {
        // The loop guard `i + 1 < UNITS.len()` must not overshoot the
        // unit table even for absurd input.
        let s = human_bytes(u64::MAX);
        assert!(s.ends_with(" PB"), "got {s}");
    }

    #[test]
    fn human_rate_zero() {
        assert_eq!(human_rate(0.0), "0 B/s");
    }

    #[test]
    fn human_rate_appends_per_second() {
        assert_eq!(human_rate(1024.0), "1.0 KB/s");
        assert_eq!(human_rate(1024.0 * 1024.0), "1.0 MB/s");
    }

    #[test]
    fn human_rate_truncates_fractional_bytes() {
        // The f64 → u64 cast floors. Sub-byte rates render as "0 B/s",
        // not a fractional byte count.
        assert_eq!(human_rate(0.7), "0 B/s");
    }
}
