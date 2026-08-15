//! Drawing primitives for the Dense view.
//!
//! Three things the rest of the UI has no use for, and which the Dense layout
//! is built out of:
//!
//! 1. [`area_graph`] — a braille area plot that addresses the **two
//!    sub-columns of a cell independently**, so a 120-column graph carries 240
//!    samples. [`crate::ui::graph`]'s `render_dots` deliberately fills both
//!    sub-columns to get a solid area at 4× vertical resolution; this one
//!    trades that vertical resolution for horizontal samples, and gets the
//!    magnitude back as colour. It also draws **mirrored** (`flip`), which is
//!    what puts upload below the shared time axis.
//! 2. [`meter`] — a bounded horizontal bar whose ramp is sampled by *position*,
//!    so the far end is the alarm colour before the value ever reaches it.
//! 3. [`panel`] — a rounded box that carries its own metadata in the border:
//!    hotkey, title, sub-label and right-hand info on the top edge, keybinds
//!    and paging on the bottom. This is what buys the layout its "no chrome
//!    rows" property — a heading that costs no row.
//!
//! This is the sibling of netwatch's `ui::dense::paint`, deliberately kept
//! structurally identical so the two Dense views stay in symmetry the way Lite
//! already does. Only the ramp *subjects* differ.
//!
//! Colours arrive as a [`Ramps`] built from the active [`Theme`]. Nothing here
//! hardcodes a hex value: the design handoff specifies literal ramps, but
//! honouring the user's theme outranks matching the reference pixel-for-pixel,
//! and the same rule already governs Lite.
//!
//! On a 16-colour theme the ramps **step** rather than blend — colour → bright
//! colour for magnitude, green → amber → red for severity. They must never
//! collapse to a single token: magnitude-as-colour and the meter whose far end
//! is already the alarm colour are the two ideas this view is built on, and a
//! screen with four flat colours on it reads as broken, not as restrained.
//! What we must not do is synthesise 24-bit RGB the user never chose, and
//! [`lerp`] guarantees that by returning its first operand unchanged for any
//! non-RGB input — the same call `graph::fade_color` already makes.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use unicode_width::UnicodeWidthStr;

use crate::ui::graph::{fade_color_inner, BRAILLE_BASE, BRAILLE_BIT};
use crate::ui::theme::Theme;

// ── ramps ───────────────────────────────────────────────────────────────────

/// A colour ramp, sampled 0.0..=1.0. Five stops on a truecolour theme, two or
/// three on a palette one — see [`Ramps::from_theme`].
#[derive(Debug, Clone)]
pub struct Ramp {
    stops: Vec<Color>,
}

impl Ramp {
    /// Sample at `f`, clamped to 0.0..=1.0.
    ///
    /// Palette stops can't be blended, so they **snap to the nearer rung**
    /// rather than falling back to the lower one. Flooring instead means a
    /// two-stop palette ramp only ever reaches its upper stop at exactly
    /// `f == 1.0`, which in a graph means the bright colour appears on
    /// essentially no cells and the trace looks like it has one colour.
    pub fn at(&self, f: f32) -> Color {
        match self.stops.len() {
            0 => Color::Reset,
            1 => self.stops[0],
            n => {
                let t = f.clamp(0.0, 1.0) * (n - 1) as f32;
                let i = t.floor() as usize;
                if i >= n - 1 {
                    return self.stops[n - 1];
                }
                let (a, b) = (self.stops[i], self.stops[i + 1]);
                let frac = t - i as f32;
                match (rgb(a), rgb(b)) {
                    (Some(_), Some(_)) => lerp(a, b, frac),
                    _ if frac >= 0.5 => b,
                    _ => a,
                }
            }
        }
    }

    /// The ramp's midpoint — used where a single representative colour is
    /// wanted (a legend, a label above the graph it describes).
    pub fn mid(&self) -> Color {
        self.at(0.55)
    }
}

/// The four ramps the Dense view draws with.
///
/// The split between magnitude and severity is the one deliberate departure
/// from btop, and it matters: btop's graphs run green→amber→red because CPU
/// load genuinely gets worse as it climbs. **Throughput doesn't.** A saturated
/// disk during a backup is working, not failing, and a CPU at 94% during a
/// compile is doing its job. So utilisation and throughput graphs ramp
/// cool→bright (busy), and only bounded values — temperature, memory
/// pressure, disk saturation — get the green→amber→red vocabulary.
///
/// Within a box `primary` is the inbound/first direction (cpu, disk read, net
/// down) and `secondary` the outbound/second one (memory, disk write, net up),
/// matching the convention Lite already uses.
#[derive(Debug, Clone)]
pub struct Ramps {
    /// Primary magnitude — cpu, disk read, net down. High = busy.
    pub primary: Ramp,
    /// Secondary magnitude — memory, disk write, net up. High = busy.
    pub secondary: Ramp,
    /// Bounded values where high genuinely IS bad. Meters only.
    pub load: Ramp,
    /// Present but not participating (idle cores, dead rows).
    pub dim: Ramp,
}

impl Ramps {
    /// Build every ramp from theme tokens.
    ///
    /// **A 16-colour theme gets a stepped ramp, not a flat one.** Collapsing
    /// these to a single token switches off the two ideas the whole view is
    /// built on — magnitude-as-colour in the graphs, and the meter whose far
    /// end is already the alarm colour — and leaves a screen with four colours
    /// on it that reads as broken rather than as restrained. ANSI expresses a
    /// ramp perfectly well; btop has done exactly this on 16-colour terminals
    /// for years. What we must *not* do is synthesise 24-bit RGB the user
    /// never chose, and [`lerp`] already guarantees that: it returns its first
    /// operand unchanged for any non-RGB input, so a named-colour ramp comes
    /// out as discrete steps instead of a blend.
    pub fn from_theme(t: &Theme) -> Self {
        Self {
            primary: magnitude_ramp(t.rx_rate, t.bg),
            // Cyan, not the magenta `tx_rate` netwatch uses for upload. In
            // syswatch the secondary direction is memory / disk write / net
            // up, and the accent token is what pairs with green here.
            secondary: magnitude_ramp(t.brand, t.bg),
            load: severity_ramp(t),
            dim: dim_ramp(t),
        }
    }
}

/// Deep-and-cool at the baseline through bright at the peak, anchored on a
/// theme token. Five stops so the eye can rank a spike without an axis.
///
/// On a palette theme there is nothing to interpolate, so the ramp becomes the
/// two rungs ANSI actually has: the colour, then its bright variant.
fn magnitude_ramp(base: Color, bg: Color) -> Ramp {
    if rgb(base).is_none() {
        // Hue at the baseline, bright at the peak — and **never grey at the
        // bottom**. A quiet machine's trace only occupies the lowest rows of
        // its box, so a grey low stop paints the entire graph the same colour
        // as the border around it, which is precisely what "looks broken"
        // means. 16 colours have no dim-green, so the honest low rung is green
        // itself.
        return Ramp {
            stops: vec![base, bright_of(base)],
        };
    }
    // The ramp runs from *low contrast with the background* to *high
    // contrast with it* —*not* from dark to light. On a dark theme those are
    // the same thing, which is why it is easy to get wrong; on a light theme
    // they are opposites, and blending the peak toward white there washes the
    // busiest part of every graph out into the page.
    let peak = if luminance(bg) > 0.5 {
        Color::Rgb(0, 0, 0)
    } else {
        Color::Rgb(255, 255, 255)
    };
    Ramp {
        stops: vec![
            fade_color_inner(base, bg, 0.30, false),
            fade_color_inner(base, bg, 0.62, false),
            base,
            lerp(base, peak, 0.42),
            lerp(base, peak, 0.76),
        ],
    }
}

/// Relative luminance, 0.0..=1.0. Non-RGB (a palette theme's `Reset`) is
/// assumed dark: that is what the overwhelming majority of terminals are, and
/// guessing light would invert the ramp for everyone who left the default.
fn luminance(c: Color) -> f32 {
    match rgb(c) {
        Some((r, g, b)) => (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0,
        None => 0.0,
    }
}

/// green → amber → red, for bounded values where high genuinely is bad.
///
/// Never collapses: these three tokens exist in every theme including the
/// 16-colour one, so a meter always tells you where the danger zone is.
fn severity_ramp(t: &Theme) -> Ramp {
    if rgb(t.status_good).is_none() {
        return Ramp {
            stops: vec![t.status_good, t.status_warn, t.status_error],
        };
    }
    Ramp {
        stops: vec![
            t.status_good,
            lerp(t.status_good, t.status_warn, 0.5),
            t.status_warn,
            lerp(t.status_warn, t.status_error, 0.5),
            t.status_error,
        ],
    }
}

/// Present but not participating. Two rungs on a palette theme so an idle
/// baseline is still distinguishable from the box furniture drawn beside it.
fn dim_ramp(t: &Theme) -> Ramp {
    if rgb(t.text_muted).is_none() {
        return Ramp {
            stops: vec![t.text_muted, t.text_secondary],
        };
    }
    Ramp {
        stops: vec![
            fade_color_inner(t.text_muted, t.bg, 0.45, false),
            t.text_muted,
            fade_color_inner(t.text_secondary, t.bg, 0.85, false),
        ],
    }
}

/// The bright half of an ANSI pair. `Indexed(0..8)` has its bright twin eight
/// slots up; anything already bright, or with no brighter form, stays put.
fn bright_of(c: Color) -> Color {
    match c {
        Color::Black => Color::DarkGray,
        Color::Red => Color::LightRed,
        Color::Green => Color::LightGreen,
        Color::Yellow => Color::LightYellow,
        Color::Blue => Color::LightBlue,
        Color::Magenta => Color::LightMagenta,
        Color::Cyan => Color::LightCyan,
        Color::Gray => Color::White,
        Color::DarkGray => Color::Gray,
        Color::Indexed(n) if n < 8 => Color::Indexed(n + 8),
        other => other,
    }
}

fn rgb(c: Color) -> Option<(u8, u8, u8)> {
    match c {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        _ => None,
    }
}

/// Linear blend, RGB only. Non-RGB inputs return `a` unchanged rather than
/// guessing at a palette entry's actual value — the same discipline as
/// [`crate::ui::graph::fade_color`].
fn lerp(a: Color, b: Color, f: f32) -> Color {
    match (rgb(a), rgb(b)) {
        (Some((ar, ag, ab)), Some((br, bg_, bb))) => {
            let f = f.clamp(0.0, 1.0);
            let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * f).round() as u8;
            Color::Rgb(mix(ar, br), mix(ag, bg_), mix(ab, bb))
        }
        _ => a,
    }
}

// ── braille area graph ──────────────────────────────────────────────────────

/// Sub-cell height of `v` against `max`, rounded.
fn sub_height(v: u64, max: u64, sub_h: usize) -> usize {
    if max == 0 {
        return 0;
    }
    let v = v.min(max) as u128;
    // Round half up in integer maths: (v * sub_h + max/2) / max.
    ((v * sub_h as u128 * 2 + max as u128) / (max as u128 * 2)) as usize
}

/// Braille area plot at two samples per cell column.
///
/// `samples` is oldest-first. The window is anchored to the **end** of the
/// slice: a ring buffer is normally longer than the graph it feeds, so the
/// right-hand edge is always *now*, blank columns appear on the left while
/// history is still filling, and the oldest samples scroll off. Reading from
/// index zero instead would silently drop the newest samples, which puts the
/// headline value off the graph and makes the trace disagree with the number
/// printed beside it.
///
/// `flip` grows the plot **downward from the top edge** instead of upward from
/// the bottom. That is the whole mirrored-graph trick: the upload half is the
/// same function with `flip = true`, sharing the axis row above it.
///
/// Every cell is coloured by its own height in the plot rather than by which
/// series it belongs to, so severity is pre-attentive — you see the spike
/// before you read the axis.
pub fn area_graph(
    buf: &mut Buffer,
    area: Rect,
    samples: &[u64],
    max: u64,
    ramp: &Ramp,
    flip: bool,
) {
    if area.width == 0 || area.height == 0 || max == 0 {
        return;
    }
    let w = area.width as usize;
    let h = area.height as usize;
    let sub_h = h * 4;
    let want = w * 2;
    // Signed offset: negative while the buffer is still filling, which leaves
    // the left-hand columns blank instead of stretching a short history.
    let off = samples.len() as isize - want as isize;
    let sample = |i: usize| -> Option<u64> {
        let idx = off + i as isize;
        if idx < 0 {
            None
        } else {
            samples.get(idx as usize).copied()
        }
    };

    for cx in 0..w {
        let (Some(lv), Some(rv)) = (sample(cx * 2), sample(cx * 2 + 1)) else {
            continue;
        };
        // A zero sample still draws its baseline dot. Skipping it leaves holes
        // in the area wherever the machine went quiet, which reads as "no
        // data" rather than "nothing happening" — and it is what stops this
        // looking like btop, whose plot is continuous across the whole window.
        let lh = sub_height(lv, max, sub_h).max(1);
        let rh = sub_height(rv, max, sub_h).max(1);
        for cy in 0..h {
            let mut bits: u8 = 0;
            for (s, (l_dot, r_dot)) in BRAILLE_BIT[0].iter().zip(BRAILLE_BIT[1]).enumerate() {
                let from_top = cy * 4 + s;
                // Depth of this sub-row measured from the growing edge.
                let depth = if flip { from_top + 1 } else { sub_h - from_top };
                if lh >= depth {
                    bits |= 1 << l_dot;
                }
                if rh >= depth {
                    bits |= 1 << r_dot;
                }
            }
            if bits == 0 {
                continue;
            }
            // Sample the ramp at the cell's vertical midpoint, measured along
            // the direction of growth, so both halves of a mirrored pair
            // brighten as the value climbs.
            let mid = (cy * 4 + 2) as f32;
            let f = if flip {
                mid / sub_h as f32
            } else {
                (sub_h as f32 - mid) / sub_h as f32
            };
            let Some(ch) = char::from_u32(BRAILLE_BASE | bits as u32) else {
                continue;
            };
            if let Some(cell) = buf.cell_mut((area.x + cx as u16, area.y + cy as u16)) {
                cell.set_char(ch);
                cell.set_style(Style::default().fg(ramp.at(f)));
            }
        }
    }
}

/// Single-row sparkline: [`area_graph`] at height 1.
///
/// A one-row cell has only eight dot levels, so callers feeding it a
/// percentage should pass values already spread by [`perceptual`] — a linear
/// map collapses everything under ~12% onto the same dot, and a 1% row then
/// looks identical to an 18% one.
pub fn spark(buf: &mut Buffer, x: u16, y: u16, w: u16, samples: &[u64], max: u64, ramp: &Ramp) {
    area_graph(buf, Rect::new(x, y, w, 1), samples, max, ramp, false);
}

/// Spread a 0..=1 fraction across eight dot levels so the low end stays
/// readable. sqrt is monotonic, so ordering still reads true — a busier row is
/// never drawn shorter than a quieter one.
pub fn perceptual(frac: f32, scale: u64) -> u64 {
    (frac.clamp(0.0, 1.0).sqrt() * scale as f32).round() as u64
}

/// A flat baseline for a series that is present but not moving — an idle core,
/// a sleeping process. Drawing nothing would read as "no such row"; a flat
/// line reads as "nothing happening", which is the truth.
pub fn baseline(buf: &mut Buffer, x: u16, y: u16, w: u16, color: Color) {
    for i in 0..w {
        if let Some(cell) = buf.cell_mut((x + i, y)) {
            cell.set_char('⣀');
            cell.set_style(Style::default().fg(color));
        }
    }
}

// ── meter ───────────────────────────────────────────────────────────────────

/// Bounded-value meter, btop's mem-bar idiom.
///
/// The ramp is sampled by **position along the bar**, not by value, so the far
/// end is always the alarm colour even when the value never reaches it — you
/// learn where the danger zone is before you're in it.
///
/// Callers must leave room for whatever label follows: a meter that runs under
/// its own readout appears to *shrink* as it fills, which is a lie told at
/// precisely the moment the number matters.
pub fn meter(buf: &mut Buffer, x: u16, y: u16, w: u16, frac: f32, ramp: &Ramp, empty: Color) {
    if w == 0 {
        return;
    }
    let filled = (frac.clamp(0.0, 1.0) * w as f32).round() as u16;
    // A 1-cell meter has no span to interpolate across; sample the ramp's
    // origin rather than dividing by zero.
    let span = (w.saturating_sub(1)).max(1) as f32;
    for i in 0..w {
        let on = i < filled;
        if let Some(cell) = buf.cell_mut((x + i, y)) {
            cell.set_char(if on { '■' } else { '·' });
            cell.set_style(Style::default().fg(if on { ramp.at(i as f32 / span) } else { empty }));
        }
    }
}

// ── panel ───────────────────────────────────────────────────────────────────

const TL: char = '╭';
const TR: char = '╮';
const BL: char = '╰';
const BR: char = '╯';
const H: char = '─';
const V: char = '│';

/// A keybind segment for the bottom border: `(key, rest)`, drawn as an accented
/// key followed by dim text — `q` + `uit`, `↑↓` + ` select`.
pub type Bind<'a> = (&'a str, &'a str);

/// Everything a [`panel`] carries in its own border.
#[derive(Default)]
pub struct PanelOpts<'a> {
    /// Bracketed hotkey at the top-left: the `1` in `╭─┤1├─┤ cpu ├`.
    pub key: Option<&'a str>,
    pub title: Option<&'a str>,
    /// Dim qualifier after the title — the CPU model, the row count.
    pub sub: Option<&'a str>,
    /// Right-hand info on the top border.
    pub right: Option<&'a str>,
    pub right_style: Option<Style>,
    /// Keybind strip on the bottom border.
    pub foot_left: &'a [Bind<'a>],
    /// Paging / range on the bottom border.
    pub foot_right: Option<&'a str>,
}

/// Draw a rounded panel and return its **interior** rect.
///
/// Rounded hairline corners read as soft furniture rather than a grid of cages
/// — the reason btop looks modern and `dialog` looks like 1994.
pub fn panel(buf: &mut Buffer, area: Rect, t: &Theme, o: &PanelOpts) -> Rect {
    if area.width < 2 || area.height < 2 {
        return area;
    }
    let border = Style::default().fg(t.border);
    let x0 = area.x;
    let y0 = area.y;
    let x1 = area.x + area.width - 1;
    let y1 = area.y + area.height - 1;

    for x in (x0 + 1)..x1 {
        set(buf, x, y0, H, border);
        set(buf, x, y1, H, border);
    }
    for y in (y0 + 1)..y1 {
        set(buf, x0, y, V, border);
        set(buf, x1, y, V, border);
    }
    set(buf, x0, y0, TL, border);
    set(buf, x1, y0, TR, border);
    set(buf, x0, y1, BL, border);
    set(buf, x1, y1, BR, border);

    // ── top border inserts ──
    let mut cx = x0 + 1;
    if o.key.is_some() || o.title.is_some() {
        // One rule cell before the first bracket, so a corner never butts an
        // insert.
        cx = put(buf, cx, y0, "─", border);
    }
    if let Some(k) = o.key {
        cx = put(buf, cx, y0, "┤", border);
        cx = put(
            buf,
            cx,
            y0,
            k,
            Style::default().fg(t.key_hint).add_modifier(Modifier::BOLD),
        );
        cx = put(buf, cx, y0, "├─", border);
    }
    if let Some(title) = o.title {
        cx = put(buf, cx, y0, "┤ ", border);
        cx = put(
            buf,
            cx,
            y0,
            title,
            Style::default().fg(t.brand).add_modifier(Modifier::BOLD),
        );
        cx = put(buf, cx, y0, " ├", border);
    }
    if let Some(sub) = o.sub {
        cx = put(buf, cx, y0, "─┤ ", border);
        cx = put(buf, cx, y0, sub, Style::default().fg(t.text_muted));
        let _ = put(buf, cx, y0, " ├", border);
    }
    if let Some(right) = o.right {
        let style = o
            .right_style
            .unwrap_or_else(|| Style::default().fg(t.text_muted));
        insert_right(buf, x1, y0, right, style, border);
    }

    // ── bottom border inserts ──
    if !o.foot_left.is_empty() {
        let mut fx = x0 + 2;
        fx = put(buf, fx, y1, "┤ ", border);
        for (i, (key, rest)) in o.foot_left.iter().enumerate() {
            if i > 0 {
                fx = put(buf, fx, y1, "  ", border);
            }
            fx = put(
                buf,
                fx,
                y1,
                key,
                Style::default().fg(t.key_hint).add_modifier(Modifier::BOLD),
            );
            fx = put(buf, fx, y1, rest, Style::default().fg(t.text_muted));
        }
        let _ = put(buf, fx, y1, " ├", border);
    }
    if let Some(fr) = o.foot_right {
        insert_right(buf, x1, y1, fr, Style::default().fg(t.text_muted), border);
    }

    Rect::new(x0 + 1, y0 + 1, area.width - 2, area.height - 2)
}

/// `┤ text ├` ending one rule cell short of the corner at `x1`.
fn insert_right(buf: &mut Buffer, x1: u16, y: u16, text: &str, style: Style, border: Style) {
    let w = text.width() as u16 + 4; // ┤ + space + text + space + ├
    if w + 2 > x1 {
        return;
    }
    let x = x1 - 1 - w;
    let mut cx = put(buf, x, y, "┤ ", border);
    cx = put(buf, cx, y, text, style);
    let _ = put(buf, cx, y, " ├", border);
}

pub fn set(buf: &mut Buffer, x: u16, y: u16, ch: char, style: Style) {
    if x >= buf.area.right() || y >= buf.area.bottom() {
        return;
    }
    if let Some(cell) = buf.cell_mut((x, y)) {
        cell.set_char(ch);
        cell.set_style(style);
    }
}

/// Write `s` at `(x, y)` and return the column after it.
pub fn put(buf: &mut Buffer, x: u16, y: u16, s: &str, style: Style) -> u16 {
    if x >= buf.area.right() || y >= buf.area.bottom() {
        return x;
    }
    let max = (buf.area.right() - x) as usize;
    buf.set_stringn(x, y, s, max, style);
    x + s.width() as u16
}

/// Write `s` so it **ends** at `x_end`. Right-aligned fields are how every
/// axis label and box-edge readout in the layout is placed; hand-picking their
/// start column is what makes them drift when a value changes width.
pub fn put_right(buf: &mut Buffer, x_end: u16, y: u16, s: &str, style: Style) -> u16 {
    let w = s.width() as u16;
    if w > x_end + 1 {
        return x_end;
    }
    put(buf, x_end + 1 - w, y, s, style)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme;

    fn buf(w: u16, h: u16) -> Buffer {
        Buffer::empty(Rect::new(0, 0, w, h))
    }

    fn ch(b: &Buffer, x: u16, y: u16) -> char {
        b[(x, y)].symbol().chars().next().unwrap_or(' ')
    }

    fn row(b: &Buffer, y: u16) -> String {
        (0..b.area.width).map(|x| ch(b, x, y)).collect()
    }

    fn ramp() -> Ramp {
        Ramp {
            stops: vec![Color::Green],
        }
    }

    /// The two sub-columns must be independently addressable — that is the
    /// entire point of this graph over `graph::render_dots`. Two different
    /// samples in one cell must produce an asymmetric glyph. (A zero sample
    /// still contributes its baseline dot, so "empty" is one dot, not none.)
    #[test]
    fn two_samples_share_one_cell() {
        let mut b = buf(1, 1);
        area_graph(&mut b, Rect::new(0, 0, 1, 1), &[4, 0], 4, &ramp(), false);
        let c = ch(&b, 0, 0) as u32;
        let bits = c - BRAILLE_BASE;
        // Left column full (dots 1,2,3,7 → bits 0,1,2,6), right at baseline
        // (dot 8 → bit 7).
        assert_eq!(bits & 0b0100_0111, 0b0100_0111, "left column not full");
        assert_eq!(bits & 0b0011_1000, 0, "right column should not be filled");
    }

    /// The right-hand edge is *now*. A buffer longer than the graph must show
    /// its newest samples, not its oldest.
    #[test]
    fn window_is_anchored_to_the_newest_samples() {
        let mut b = buf(2, 1);
        // 8 samples into a 2-wide (4-sample) graph: the last four are 0,0,4,4.
        area_graph(
            &mut b,
            Rect::new(0, 0, 2, 1),
            &[4, 4, 4, 4, 0, 0, 4, 4],
            4,
            &ramp(),
            false,
        );
        let left = ch(&b, 0, 0) as u32 - BRAILLE_BASE;
        let right = ch(&b, 1, 0) as u32 - BRAILLE_BASE;
        assert_eq!(
            left.count_ones(),
            2,
            "oldest visible pair should be baseline"
        );
        assert_eq!(right.count_ones(), 8, "newest pair should be full");
    }

    /// A half-full buffer leaves the LEFT blank — history that hasn't happened
    /// yet must not be faked by stretching what has.
    #[test]
    fn partial_history_leaves_the_left_blank() {
        let mut b = buf(4, 1);
        area_graph(&mut b, Rect::new(0, 0, 4, 1), &[4, 4], 4, &ramp(), false);
        assert_eq!(ch(&b, 0, 0), ' ');
        assert_eq!(ch(&b, 1, 0), ' ');
        assert_eq!(ch(&b, 2, 0), ' ');
        assert_ne!(
            ch(&b, 3, 0),
            ' ',
            "newest pair should occupy the right edge"
        );
    }

    /// `flip` must grow from the top edge, so a mirrored pair meets at a
    /// shared axis instead of both hugging the bottom.
    #[test]
    fn flip_grows_downward() {
        let mut up = buf(1, 2);
        let mut down = buf(1, 2);
        area_graph(&mut up, Rect::new(0, 0, 1, 2), &[2, 2], 8, &ramp(), false);
        area_graph(&mut down, Rect::new(0, 0, 1, 2), &[2, 2], 8, &ramp(), true);
        assert_eq!(ch(&up, 0, 0), ' ', "unflipped must not touch the top row");
        assert_eq!(
            ch(&down, 0, 1),
            ' ',
            "flipped must not touch the bottom row"
        );
    }

    /// The magnitude ramp must gain contrast against the background as it
    /// climbs. On a light theme that means getting DARKER — blending the peak
    /// toward white there washes the busiest part of every graph into the page.
    #[test]
    fn magnitude_ramp_gains_contrast_against_the_background() {
        for name in ["dark", "light", "ocean", "solarized", "dracula", "nord"] {
            let t = theme::by_name(name);
            let r = Ramps::from_theme(&t);
            let bg = t.bg;
            let low = contrast(r.primary.at(0.0), bg);
            let high = contrast(r.primary.at(1.0), bg);
            assert!(
                high > low,
                "{name}: peak contrast {high:.2} not above baseline {low:.2}"
            );
            assert!(
                high > 2.0,
                "{name}: peak contrast {high:.2} too low to read"
            );
        }
    }

    fn contrast(a: Color, b: Color) -> f32 {
        let l = |c: Color| match c {
            Color::Rgb(r, g, bl) => {
                (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * bl as f32) / 255.0
            }
            _ => 0.0,
        };
        let (x, y) = (l(a), l(b));
        let (hi, lo) = if x > y { (x, y) } else { (y, x) };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn meter_far_end_is_alarm_coloured_before_the_value_reaches_it() {
        let t = theme::active();
        let ramps = Ramps::from_theme(&t);
        let mut b = buf(10, 1);
        meter(&mut b, 0, 0, 10, 0.3, &ramps.load, t.text_muted);
        assert_eq!(row(&b, 0), "■■■·······");
        // Position-sampled: the last filled cell of a 30% bar is not the same
        // colour as the last cell of a 100% bar.
        let mut full = buf(10, 1);
        meter(&mut full, 0, 0, 10, 1.0, &ramps.load, t.text_muted);
        assert_ne!(b[(2, 0)].fg, full[(9, 0)].fg);
    }

    #[test]
    fn meter_of_width_one_does_not_divide_by_zero() {
        let t = theme::active();
        let ramps = Ramps::from_theme(&t);
        let mut b = buf(1, 1);
        meter(&mut b, 0, 0, 1, 0.5, &ramps.load, t.text_muted);
        assert_eq!(ch(&b, 0, 0), '■');
    }

    #[test]
    fn panel_carries_its_metadata_in_the_border() {
        let t = theme::active();
        let mut b = buf(40, 3);
        let inner = panel(
            &mut b,
            Rect::new(0, 0, 40, 3),
            &t,
            &PanelOpts {
                key: Some("1"),
                title: Some("cpu"),
                right: Some("up 4d"),
                ..Default::default()
            },
        );
        let top = row(&b, 0);
        assert!(top.starts_with("╭─┤1├─┤ cpu ├"), "got {top:?}");
        assert!(top.contains("┤ up 4d ├"), "got {top:?}");
        assert!(top.ends_with('╮'), "got {top:?}");
        assert_eq!(inner, Rect::new(1, 1, 38, 1));
    }

    /// The 16-colour contract: a palette theme still gets a real ramp, and
    /// **no synthesised RGB** appears anywhere in it. Collapsing these to one
    /// flat token is what made the whole view look broken on `theme =
    /// "terminal"`.
    #[test]
    fn palette_theme_ramps_step_without_inventing_rgb() {
        let t = theme::by_name("terminal");
        let r = Ramps::from_theme(&t);
        for ramp in [&r.primary, &r.secondary, &r.load, &r.dim] {
            let seen: Vec<Color> = (0..=10).map(|i| ramp.at(i as f32 / 10.0)).collect();
            assert!(
                seen.iter().any(|c| *c != seen[0]),
                "ramp collapsed to a single colour on a palette theme"
            );
            assert!(
                !seen.iter().any(|c| matches!(c, Color::Rgb(..))),
                "palette theme must never emit synthesised RGB, got {seen:?}"
            );
        }
        // The meter's far end must still be the alarm colour.
        assert_eq!(r.load.at(0.0), t.status_good);
        assert_eq!(r.load.at(1.0), t.status_error);
    }

    /// An RGB theme keeps the smooth five-stop blend.
    #[test]
    fn rgb_theme_ramps_interpolate() {
        let t = theme::by_name("dark");
        let r = Ramps::from_theme(&t);
        let stops: Vec<Color> = (0..=8).map(|i| r.primary.at(i as f32 / 8.0)).collect();
        let distinct: std::collections::BTreeSet<String> =
            stops.iter().map(|c| format!("{c:?}")).collect();
        assert!(distinct.len() >= 7, "expected a smooth ramp, got {stops:?}");
    }

    /// Secondary is the accent, not netwatch's magenta upload token — memory
    /// and disk write should read cyan against the primary green.
    #[test]
    fn secondary_ramp_follows_the_accent_not_tx_rate() {
        let t = theme::by_name("dark");
        let r = Ramps::from_theme(&t);
        assert_ne!(r.secondary.at(0.5), t.tx_rate);
        assert_eq!(r.secondary.at(0.5), t.brand);
    }

    #[test]
    fn perceptual_is_monotonic_and_lifts_the_low_end() {
        assert!(perceptual(0.01, 8) <= perceptual(0.18, 8));
        assert!(perceptual(0.18, 8) < perceptual(0.9, 8));
        // The point of the transform: 1% and 18% must not collapse onto one
        // dot the way a linear map would.
        assert_ne!(perceptual(0.01, 8), perceptual(0.18, 8));
    }
}
