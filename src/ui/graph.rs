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
//!
//! `GraphOpts.fade` enables a btop-style right-bright / left-dim gradient on
//! every column plus a faint dot grid behind the data. Call sites build the
//! opts from `App::graph_opts()` so a single config toggle governs the
//! entire UI.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::Span,
    Frame,
};

use crate::ui::palette as p;
use crate::ui::theme;

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

/// Cross-cutting render preferences passed to every chart call site.
/// Fade + grid travel together because users want btop's whole look or
/// none of it.
#[derive(Debug, Clone, Copy)]
pub struct GraphOpts {
    /// Apply right-bright / left-dim color gradient per column AND draw a
    /// faint dot grid behind the data. Off → original solid-color look.
    pub fade: bool,
}

impl Default for GraphOpts {
    fn default() -> Self {
        Self { fade: false }
    }
}

/// Lowest fraction of the base color the leftmost (oldest) column receives
/// when fade is on. 0.30 keeps the data visible without dominating.
const MIN_FADE_ALPHA: f32 = 0.30;

/// Lowest fraction of a table row's foreground color the bottommost
/// visible row receives. Higher than the chart fade (0.55 vs 0.30) so
/// table text stays legible at the dim end.
const MIN_ROW_FADE_ALPHA: f32 = 0.55;

/// Smallest chart (cells) where the grid overlay renders. Narrower tiles
/// would look noisy with grid dots overlapping the data.
const GRID_MIN_W: u16 = 16;
const GRID_MIN_H: u16 = 4;

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
pub fn render(
    f: &mut Frame,
    area: Rect,
    samples: &[f32],
    style: GraphStyle,
    color: Color,
    opts: GraphOpts,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    if opts.fade && area.width >= GRID_MIN_W && area.height >= GRID_MIN_H {
        render_grid(f.buffer_mut(), area);
    }
    match style {
        GraphStyle::Bars => render_bars(f.buffer_mut(), area, samples, color, opts),
        GraphStyle::Dots => render_dots(f.buffer_mut(), area, samples, color, opts),
    }
}

fn render_bars(buf: &mut Buffer, area: Rect, samples: &[f32], base: Color, opts: GraphOpts) {
    let cell_w = area.width as usize;
    let cell_h = area.height as usize;
    if cell_w == 0 || cell_h == 0 || samples.is_empty() {
        return;
    }
    let take = cell_w;
    let slice: &[f32] = if samples.len() > take {
        &samples[samples.len() - take..]
    } else {
        samples
    };
    let n = slice.len();
    let n_minus_1 = n.saturating_sub(1).max(1) as f32;

    // Paint the full chart area with the theme bg first. The pre-fade
    // implementation used a `Paragraph::new(lines).style(.bg(p::bg()))`
    // which covered every cell, including columns past `samples.len()`
    // when the history was short. The per-cell loop below only touches
    // data columns, so without this pass empty leading columns would
    // keep whatever was in the buffer (typically the terminal default —
    // visually close to the theme bg on dark themes but not identical).
    // Skip when fade is on so we don't erase the grid render that ran
    // just before us.
    if !opts.fade {
        for y in 0..cell_h {
            for x in 0..cell_w {
                if let Some(cell) = buf.cell_mut((area.x + x as u16, area.y + y as u16)) {
                    cell.set_char(' ');
                    cell.set_style(Style::default().bg(p::bg()));
                }
            }
        }
    }

    // When fade is off this matches the original implementation exactly:
    // every cell in a column gets the same glyph & color, tiled vertically.
    for (i, &v) in slice.iter().enumerate() {
        let v = v.clamp(0.0, 1.0);
        let idx = ((v * 7.0).round() as usize).min(7);
        let glyph = BLOCK_GLYPHS[idx];
        let color = if opts.fade {
            let alpha = MIN_FADE_ALPHA + (1.0 - MIN_FADE_ALPHA) * (i as f32 / n_minus_1);
            fade_color(base, p::bg(), alpha)
        } else {
            base
        };
        let x_offset = cell_w.saturating_sub(n) + i;
        let x = area.x + x_offset as u16;
        for cy in 0..cell_h {
            if let Some(cell) = buf.cell_mut((x, area.y + cy as u16)) {
                cell.set_char(glyph);
                cell.set_style(Style::default().fg(color).bg(p::bg()));
            }
        }
    }
}

fn render_dots(buf: &mut Buffer, area: Rect, samples: &[f32], color: Color, opts: GraphOpts) {
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
    // When fade is on the grid pass already touched the area; this re-paints
    // every cell, so don't run BG-paint when fade is enabled or we'd erase
    // the grid.
    if !opts.fade {
        for y in 0..cell_h {
            for x in 0..cell_w {
                if let Some(cell) = buf.cell_mut((area.x + x as u16, area.y + y as u16)) {
                    cell.set_char(' ');
                    cell.set_style(Style::default().bg(p::bg()));
                }
            }
        }
    }

    let mut masks = vec![vec![0u8; cell_w]; cell_h];

    for (i, &v) in slice.iter().enumerate() {
        let v = v.clamp(0.0, 1.0);
        if v <= 0.0 {
            continue;
        }
        let top_pixel_from_bottom = ((v * (pix_h as f32 - 1.0)).round() as usize).min(pix_h - 1);
        for fill in 0..=top_pixel_from_bottom {
            let pix_y_from_top = (pix_h - 1) - fill;
            let cell_y = pix_y_from_top / 4;
            let row_in_cell = pix_y_from_top % 4;
            masks[cell_y][i] |= 1 << BRAILLE_BIT[0][row_in_cell];
            masks[cell_y][i] |= 1 << BRAILLE_BIT[1][row_in_cell];
        }
    }

    let n = slice.len();
    let n_minus_1 = n.saturating_sub(1).max(1) as f32;

    for (y, row) in masks.iter().enumerate() {
        for (x, &mask) in row.iter().enumerate() {
            if mask == 0 {
                continue;
            }
            let cell_color = if opts.fade {
                let alpha = MIN_FADE_ALPHA + (1.0 - MIN_FADE_ALPHA) * (x as f32 / n_minus_1);
                fade_color(color, p::bg(), alpha)
            } else {
                color
            };
            let ch = char::from_u32(BRAILLE_BASE | mask as u32).unwrap_or(' ');
            if let Some(cell) = buf.cell_mut((area.x + x as u16, area.y + y as u16)) {
                cell.set_char(ch);
                cell.set_style(Style::default().fg(cell_color).bg(p::bg()));
            }
        }
    }
}

// ── fade + grid helpers ─────────────────────────────────────────────────────

/// Linear-interpolate from `bg` toward `base` at fraction `alpha`. Uses RGB
/// interpolation; ANSI named colors are resolved via a standard xterm
/// palette so the default theme (which uses Color::Green, Color::Cyan, …)
/// still preserves its hue at the dim end instead of fading to grayscale.
pub fn fade_color(base: Color, bg: Color, alpha: f32) -> Color {
    fade_color_inner(base, bg, alpha, theme::active().name == "terminal")
}

/// The decision split out from the theme lookup so it can be tested without
/// mutating the global active theme (which would race the other fade tests).
///
/// `defer_to_terminal` short-circuits the interpolation: the `terminal`
/// theme's whole contract is that syswatch emits nothing but the user's own
/// palette, and interpolating produces 24-bit RGB for every faded cell —
/// with fade on, the majority of the frame. There is no 16-color equivalent
/// of a gradient, so fade degrades to off rather than to wrong and the base
/// color passes through untouched. `bg` is also `Reset` under that theme, so
/// the real background is unknown; fading toward an assumed black would be
/// wrong on a light terminal regardless.
fn fade_color_inner(base: Color, bg: Color, alpha: f32, defer_to_terminal: bool) -> Color {
    if defer_to_terminal {
        return base;
    }
    let alpha = alpha.clamp(0.0, 1.0);
    let (br, bgc, bb) = to_rgb_or_default(base, (255, 255, 255));
    let (gr, gg, gb) = to_rgb_or_default(bg, (0, 0, 0));
    Color::Rgb(
        lerp_u8(gr, br, alpha),
        lerp_u8(gg, bgc, alpha),
        lerp_u8(gb, bb, alpha),
    )
}

fn to_rgb_or_default(c: Color, fallback: (u8, u8, u8)) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Reset => fallback,
        Color::Black => (0, 0, 0),
        Color::Red => (170, 0, 0),
        Color::Green => (0, 170, 0),
        Color::Yellow => (170, 85, 0),
        Color::Blue => (0, 0, 170),
        Color::Magenta => (170, 0, 170),
        Color::Cyan => (0, 170, 170),
        Color::Gray => (170, 170, 170),
        Color::DarkGray => (85, 85, 85),
        Color::LightRed => (255, 85, 85),
        Color::LightGreen => (85, 255, 85),
        Color::LightYellow => (255, 255, 85),
        Color::LightBlue => (85, 85, 255),
        Color::LightMagenta => (255, 85, 255),
        Color::LightCyan => (85, 255, 255),
        Color::White => (255, 255, 255),
        _ => fallback,
    }
}

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t)
        .round()
        .clamp(0.0, 255.0) as u8
}

/// Linear alpha for the `row_idx`-th visible row in a table of `total_rows`
/// rows. Row 0 → 1.0, last row → `MIN_ROW_FADE_ALPHA`. Single-row tables
/// return 1.0 (avoid div-by-zero edge case).
pub fn row_fade_alpha(row_idx: usize, total_rows: usize) -> f32 {
    if total_rows <= 1 {
        return 1.0;
    }
    let denom = (total_rows - 1) as f32;
    1.0 - (1.0 - MIN_ROW_FADE_ALPHA) * (row_idx as f32 / denom)
}

/// Map over every span in `spans`, blending each span's fg toward `bg` at
/// `alpha`. Spans without an explicit fg are left untouched so unstyled
/// text doesn't suddenly pick up a fade color it wasn't supposed to have.
pub fn fade_spans_fg<'a>(spans: Vec<Span<'a>>, bg: Color, alpha: f32) -> Vec<Span<'a>> {
    spans
        .into_iter()
        .map(|mut s| {
            if let Some(fg) = s.style.fg {
                s.style = s.style.fg(fade_color(fg, bg, alpha));
            }
            s
        })
        .collect()
}

/// Faint dot grid behind the chart. Renders before the data so any data
/// cell overwrites a grid cell; the empty regions show the grid through.
fn render_grid(buf: &mut Buffer, area: Rect) {
    // The grid's base is a fixed grey, so it would survive `fade_color`'s
    // terminal-theme passthrough as raw RGB. Use the palette's own chrome
    // slot instead, which is where something this quiet belongs anyway.
    let grid_color = if theme::active().name == "terminal" {
        p::border()
    } else {
        fade_color(Color::Rgb(150, 150, 150), p::bg(), 0.20)
    };
    let cell_w = area.width as usize;
    let cell_h = area.height as usize;
    let v_step = (cell_w / 4).max(2);
    let h_step = (cell_h / 4).max(1);

    for x in (v_step..cell_w).step_by(v_step) {
        for cy in 0..cell_h {
            if let Some(cell) = buf.cell_mut((area.x + x as u16, area.y + cy as u16)) {
                cell.set_char('·');
                cell.set_style(Style::default().fg(grid_color).bg(p::bg()));
            }
        }
    }
    for y in (h_step..cell_h).step_by(h_step) {
        for cx in 0..cell_w {
            if let Some(cell) = buf.cell_mut((area.x + cx as u16, area.y + y as u16)) {
                if cell.symbol() != "·" {
                    cell.set_char('·');
                    cell.set_style(Style::default().fg(grid_color).bg(p::bg()));
                }
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
        render_dots(
            &mut buf,
            area,
            &[1.0, 0.5, 0.25, 0.0],
            Color::White,
            GraphOpts::default(),
        );
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
        render_dots(&mut buf, area, &[1.0], Color::White, GraphOpts::default());
    }

    #[test]
    fn fade_passes_base_through_untouched_for_terminal_theme() {
        // With fade on, interpolation is applied to most of the frame. If
        // it ran under the `terminal` theme every faded cell would emit
        // 24-bit RGB and the theme would stop honoring the user's palette —
        // measured at 623 truecolor escapes per frame before this guard.
        let base = Color::Cyan;
        for alpha in [0.0, 0.3, 0.55, 1.0] {
            assert_eq!(
                fade_color_inner(base, Color::Reset, alpha, true),
                base,
                "alpha {alpha} must pass through under the terminal theme"
            );
        }
        // Every other theme keeps the gradient.
        assert!(matches!(
            fade_color_inner(Color::Rgb(200, 100, 50), Color::Rgb(0, 0, 0), 0.5, false),
            Color::Rgb(..)
        ));
    }

    #[test]
    fn fade_color_endpoints_match_inputs() {
        let base = Color::Rgb(200, 100, 50);
        let bg = Color::Rgb(0, 0, 0);
        assert_eq!(fade_color(base, bg, 1.0), base);
        assert_eq!(fade_color(base, bg, 0.0), bg);
    }

    #[test]
    fn fade_named_green_against_reset_bg_stays_green() {
        // Same regression as netwatch v0.21 — named colors must map to
        // sensible RGB or fade ends up grayscale instead of preserving hue.
        let dim = fade_color(Color::Green, Color::Reset, 0.3);
        match dim {
            Color::Rgb(r, g, b) => {
                assert_eq!(r, 0);
                assert!(g > 0 && g < 170);
                assert_eq!(b, 0);
            }
            _ => panic!("expected Rgb, got {:?}", dim),
        }
    }

    #[test]
    fn row_fade_alpha_endpoints_and_midpoint() {
        assert!((row_fade_alpha(0, 10) - 1.0).abs() < 1e-6);
        assert!((row_fade_alpha(9, 10) - MIN_ROW_FADE_ALPHA).abs() < 1e-6);
        assert!((row_fade_alpha(0, 1) - 1.0).abs() < 1e-6); // single row
    }

    #[test]
    fn bars_with_fade_produces_different_colors_left_to_right() {
        // Regression guard: if a refactor accidentally drops the `if
        // opts.fade` branch in render_bars, this test catches it because
        // the left and right cells would end up with identical colors.
        let area = Rect::new(0, 0, 8, 1);
        let mut buf = Buffer::empty(area);
        let samples = vec![1.0_f32; 8];
        render_bars(
            &mut buf,
            area,
            &samples,
            Color::Rgb(200, 100, 50),
            GraphOpts { fade: true },
        );
        let left_fg = buf.cell((0u16, 0u16)).unwrap().fg;
        let right_fg = buf.cell((7u16, 0u16)).unwrap().fg;
        assert_ne!(
            left_fg, right_fg,
            "fade on should produce a per-column gradient; got the same fg on both ends"
        );
        // Right edge should be the full base color (alpha=1.0).
        assert_eq!(right_fg, Color::Rgb(200, 100, 50));
    }

    #[test]
    fn bars_without_fade_uses_uniform_color() {
        let area = Rect::new(0, 0, 8, 1);
        let mut buf = Buffer::empty(area);
        let samples = vec![1.0_f32; 8];
        render_bars(
            &mut buf,
            area,
            &samples,
            Color::Rgb(200, 100, 50),
            GraphOpts::default(),
        );
        let left_fg = buf.cell((0u16, 0u16)).unwrap().fg;
        let right_fg = buf.cell((7u16, 0u16)).unwrap().fg;
        assert_eq!(left_fg, right_fg);
        assert_eq!(left_fg, Color::Rgb(200, 100, 50));
    }
}
