//! Pluggable graph rendering for the multi-row chart tiles.
//!
//! Mirrors netwatch's `graph` module: a `GraphStyle` enum plus a `render`
//! entry point that dispatches to a per-style implementation. Every multi-row
//! sparkline in the app (CPU/Net/Disk aggregate strips, Overview KPI tiles)
//! routes through here so a single toggle (`g`) flips them all.
//!
//! Inputs are pre-normalized `f32` samples in `0..=1` to match what call sites
//! already compute. One-line inline sparklines (timeline strips) keep using
//! `widgets::sparkline` directly because they composite with labels/cursors.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::ui::palette as p;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphStyle {
    /// Solid stacked block glyphs `▁▂▃▄▅▆▇█`, tiled across the full row count.
    Bars,
    /// btop-style braille area plot: each column filled bottom-up to the
    /// sample's pixel height. 4× vertical resolution over Bars.
    Dots,
}

impl GraphStyle {
    pub fn label(self) -> &'static str {
        match self {
            GraphStyle::Bars => "bars",
            GraphStyle::Dots => "dots",
        }
    }
    pub fn next(self) -> GraphStyle {
        match self {
            GraphStyle::Bars => GraphStyle::Dots,
            GraphStyle::Dots => GraphStyle::Bars,
        }
    }
}

const BLOCK_GLYPHS: [char; 8] = [
    '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}', '\u{2588}',
];

// Bit position in a braille cell mask for each (sub_col, sub_row).
// Braille pattern dots numbered 1–8 map to bits 0–7; the 4th row uses dots
// 7 and 8 (bits 6 and 7), which is why it's not a straight `row + col*4`.
const BRAILLE_BIT: [[u8; 4]; 2] = [
    [0, 1, 2, 6], // sub_col 0: rows 0..=3 → dots 1, 2, 3, 7
    [3, 4, 5, 7], // sub_col 1: rows 0..=3 → dots 4, 5, 6, 8
];
const BRAILLE_BASE: u32 = 0x2800;

/// Render `samples` (each clamped to `0..=1`) into `area` using `style`.
pub fn render(f: &mut Frame, area: Rect, samples: &[f32], style: GraphStyle, color: Color) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    match style {
        GraphStyle::Bars => render_bars(f, area, samples, color),
        GraphStyle::Dots => render_dots(f.buffer_mut(), area, samples, color),
    }
}

fn render_bars(f: &mut Frame, area: Rect, samples: &[f32], color: Color) {
    let take = area.width as usize;
    let slice: &[f32] = if samples.len() > take {
        &samples[samples.len() - take..]
    } else {
        samples
    };
    let s: String = slice
        .iter()
        .map(|v| {
            let v = v.clamp(0.0, 1.0);
            let idx = ((v * 7.0).round() as usize).min(7);
            BLOCK_GLYPHS[idx]
        })
        .collect();
    let lines: Vec<Line> = (0..area.height)
        .map(|_| Line::from(Span::styled(s.clone(), Style::default().fg(color))))
        .collect();
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(p::bg())),
        area,
    );
}

fn render_dots(buf: &mut Buffer, area: Rect, samples: &[f32], color: Color) {
    let cell_w = area.width as usize;
    let cell_h = area.height as usize;
    if cell_w == 0 || cell_h == 0 || samples.is_empty() {
        return;
    }
    let pix_h = cell_h * 4;

    let take = cell_w;
    let slice: &[f32] = if samples.len() > take {
        &samples[samples.len() - take..]
    } else {
        samples
    };

    // Paint BG across the area first so partial fills sit on a clean ground.
    for y in 0..cell_h {
        for x in 0..cell_w {
            if let Some(cell) = buf.cell_mut((area.x + x as u16, area.y + y as u16)) {
                cell.set_char(' ');
                cell.set_style(Style::default().bg(p::bg()));
            }
        }
    }

    let mut masks = vec![vec![0u8; cell_w]; cell_h];

    for (i, &v) in slice.iter().enumerate() {
        let v = v.clamp(0.0, 1.0);
        if v <= 0.0 {
            continue;
        }
        // Highest pixel-row from the bottom that this sample reaches.
        let top_pixel_from_bottom = ((v * (pix_h as f32 - 1.0)).round() as usize).min(pix_h - 1);
        for fill in 0..=top_pixel_from_bottom {
            let pix_y_from_top = (pix_h - 1) - fill;
            let cell_y = pix_y_from_top / 4;
            let row_in_cell = pix_y_from_top % 4;
            // Light both sub-columns so each filled cell-column is one cell wide.
            masks[cell_y][i] |= 1 << BRAILLE_BIT[0][row_in_cell];
            masks[cell_y][i] |= 1 << BRAILLE_BIT[1][row_in_cell];
        }
    }

    for (y, row) in masks.iter().enumerate() {
        for (x, &mask) in row.iter().enumerate() {
            if mask == 0 {
                continue;
            }
            let ch = char::from_u32(BRAILLE_BASE | mask as u32).unwrap_or(' ');
            if let Some(cell) = buf.cell_mut((area.x + x as u16, area.y + y as u16)) {
                cell.set_char(ch);
                cell.set_style(Style::default().fg(color).bg(p::bg()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_cycles_styles() {
        assert_eq!(GraphStyle::Bars.next(), GraphStyle::Dots);
        assert_eq!(GraphStyle::Dots.next(), GraphStyle::Bars);
    }

    #[test]
    fn label_is_stable() {
        assert_eq!(GraphStyle::Bars.label(), "bars");
        assert_eq!(GraphStyle::Dots.label(), "dots");
    }

    #[test]
    fn dots_writes_braille_chars_for_nonzero_samples() {
        let area = Rect::new(0, 0, 4, 2);
        let mut buf = Buffer::empty(area);
        render_dots(&mut buf, area, &[1.0, 0.5, 0.25, 0.0], Color::White);
        // First column at v=1.0 should fill the top cell with a braille glyph.
        let top_left = buf
            .cell((0u16, 0u16))
            .unwrap()
            .symbol()
            .chars()
            .next()
            .unwrap();
        assert!(
            (top_left as u32) >= BRAILLE_BASE && (top_left as u32) < BRAILLE_BASE + 256,
            "expected braille at top-left, got {:?}",
            top_left
        );
        // Last column is zero — no braille mask, just the BG-filled space.
        let zero_top = buf
            .cell((3u16, 0u16))
            .unwrap()
            .symbol()
            .chars()
            .next()
            .unwrap();
        assert_eq!(zero_top, ' ');
    }

    #[test]
    fn dots_handles_zero_area() {
        let area = Rect::new(0, 0, 0, 0);
        let mut buf = Buffer::empty(Rect::new(0, 0, 1, 1));
        // Should not panic.
        render_dots(&mut buf, area, &[1.0], Color::White);
    }
}
