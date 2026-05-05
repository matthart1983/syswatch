//! Color themes for the syswatch TUI.
//!
//! Mirrors netwatch's theme conventions: the same semantic field set and
//! the same six built-in palettes. Two syswatch-specific extensions:
//! `bg` (panel fill — netwatch keeps the terminal background, syswatch
//! paints panels) and `warn_bg` / `err_bg` (insights row tints).
//!
//! A global `RwLock<Theme>` holds the active theme; `palette::*` accessors
//! delegate here, so `theme::set` / `theme::cycle` re-color the whole UI on
//! the next draw.

use ratatui::style::Color;
use std::sync::RwLock;

/// Semantic color slots — netwatch-compatible plus syswatch panel-bg extensions.
///
/// All netwatch slots are kept intentionally — even `text_secondary`,
/// `text_inverse`, `rx_rate`, `highlight_bg`, and `separator` which the
/// current syswatch UI doesn't read yet. Keeping them present means
/// per-theme palettes don't have to change shape when new UI lands.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub name: &'static str,

    // ── Brand / chrome ──────────────────────────────────
    pub brand: Color,
    pub active_tab: Color,
    pub inactive_tab: Color,
    pub border: Color,
    pub separator: Color,

    // ── Text ────────────────────────────────────────────
    pub text_primary: Color,
    pub text_secondary: Color,
    pub text_muted: Color,
    pub text_inverse: Color,

    // ── Status / semantic ───────────────────────────────
    pub status_good: Color,
    pub status_warn: Color,
    pub status_error: Color,
    pub status_info: Color,

    // ── Data ────────────────────────────────────────────
    pub rx_rate: Color,
    pub tx_rate: Color,
    pub key_hint: Color,

    // ── Selection ───────────────────────────────────────
    pub selection_bg: Color,
    pub highlight_bg: Color,

    // ── syswatch extensions ─────────────────────────────
    /// Panel fill. Netwatch leaves bg to the terminal; syswatch paints
    /// every panel for the dense-dashboard look.
    pub bg: Color,
    /// Row tint behind a Warn-severity insight.
    pub warn_bg: Color,
    /// Row tint behind a Crit-severity insight.
    pub err_bg: Color,
}

pub const THEME_NAMES: &[&str] = &["dark", "light", "ocean", "solarized", "dracula", "nord"];

pub const fn dark() -> Theme {
    // Preserve the original syswatch palette exactly, mapped to the
    // new semantic slots. Brand/active_tab pick up the original cyan;
    // status_info shares brand (as it does in netwatch's dark).
    Theme {
        name: "dark",
        brand: Color::Rgb(0x5f, 0xdc, 0xff),
        active_tab: Color::Rgb(0x5f, 0xdc, 0xff),
        inactive_tab: Color::Rgb(0xc5, 0xd1, 0xd6),
        border: Color::Rgb(0x44, 0x56, 0x60),
        separator: Color::Rgb(0x44, 0x56, 0x60),
        text_primary: Color::Rgb(0xc5, 0xd1, 0xd6),
        text_secondary: Color::Rgb(0xa0, 0xb0, 0xb6),
        text_muted: Color::Rgb(0x6b, 0x80, 0x88),
        text_inverse: Color::Rgb(0x0c, 0x14, 0x18),
        status_good: Color::Rgb(0x5c, 0xd9, 0x89),
        status_warn: Color::Rgb(0xf0, 0xc0, 0x60),
        status_error: Color::Rgb(0xff, 0x78, 0x78),
        status_info: Color::Rgb(0x5f, 0xdc, 0xff),
        rx_rate: Color::Rgb(0x5c, 0xd9, 0x89),
        tx_rate: Color::Rgb(0xd9, 0x7a, 0xff),
        key_hint: Color::Rgb(0x5f, 0xdc, 0xff),
        selection_bg: Color::Rgb(0x1a, 0x33, 0x40),
        highlight_bg: Color::Rgb(0x2a, 0x4a, 0x5a),
        bg: Color::Rgb(0x0c, 0x14, 0x18),
        warn_bg: Color::Rgb(0x3a, 0x2c, 0x14),
        err_bg: Color::Rgb(0x3a, 0x1c, 0x1c),
    }
}

pub const fn light() -> Theme {
    // Contrast budget against the near-white bg (#f5f5f2):
    //   text_primary   #1e1e1e  ≈ 14.5:1  AAA
    //   text_secondary #404040  ≈ 9.3:1   AAA
    //   text_muted     #5a5a5a  ≈ 5.7:1   AA  (was #8c8c8c, ~3.1:1, failed)
    //   inactive_tab   #5a5a64  ≈ 5.6:1   AA  (was #8c8c8c, illegible on bg)
    //   border         #a0a0a0  ≈ 2.5:1   chrome-only, intentionally faint
    Theme {
        name: "light",
        brand: Color::Rgb(0, 100, 160),
        active_tab: Color::Rgb(180, 100, 0),
        inactive_tab: Color::Rgb(90, 90, 100),
        border: Color::Rgb(160, 160, 160),
        separator: Color::Rgb(160, 160, 160),
        text_primary: Color::Rgb(30, 30, 30),
        text_secondary: Color::Rgb(64, 64, 64),
        text_muted: Color::Rgb(90, 90, 90),
        text_inverse: Color::Rgb(255, 255, 255),
        status_good: Color::Rgb(0, 120, 50),
        status_warn: Color::Rgb(170, 110, 0),
        status_error: Color::Rgb(180, 30, 30),
        status_info: Color::Rgb(0, 100, 160),
        rx_rate: Color::Rgb(0, 120, 50),
        tx_rate: Color::Rgb(0, 80, 160),
        key_hint: Color::Rgb(180, 100, 0),
        selection_bg: Color::Rgb(220, 230, 240),
        highlight_bg: Color::Rgb(200, 215, 230),
        bg: Color::Rgb(0xf5, 0xf5, 0xf2),
        warn_bg: Color::Rgb(0xfa, 0xf0, 0xd6),
        err_bg: Color::Rgb(0xfa, 0xdc, 0xdc),
    }
}

/// Apple Terminal.app "Ocean" profile (deep blue bg).
pub const fn ocean() -> Theme {
    let bright_white = Color::Rgb(0xFF, 0xFF, 0xFF);
    let white = Color::Rgb(0xCB, 0xCC, 0xCD);
    let muted_readable = Color::Rgb(0xB5, 0xB6, 0xB7);
    let bright_red = Color::Rgb(0xFC, 0x39, 0x1F);
    let bright_green = Color::Rgb(0x31, 0xE7, 0x22);
    let bright_yellow = Color::Rgb(0xEA, 0xEC, 0x23);
    let bright_cyan = Color::Rgb(0x14, 0xF0, 0xF0);
    Theme {
        name: "ocean",
        brand: bright_cyan,
        active_tab: bright_white,
        inactive_tab: white,
        border: muted_readable,
        separator: muted_readable,
        text_primary: bright_white,
        text_secondary: white,
        text_muted: muted_readable,
        text_inverse: Color::Rgb(0, 0, 0),
        status_good: bright_green,
        status_warn: bright_yellow,
        status_error: bright_red,
        status_info: bright_cyan,
        rx_rate: bright_green,
        tx_rate: bright_cyan,
        key_hint: bright_yellow,
        selection_bg: Color::Rgb(0x21, 0x6D, 0xFF),
        highlight_bg: Color::Rgb(0x3A, 0x6B, 0xE8),
        bg: Color::Rgb(0x22, 0x4F, 0xBC),
        warn_bg: Color::Rgb(0x3A, 0x4A, 0x12),
        err_bg: Color::Rgb(0x4A, 0x1F, 0x1F),
    }
}

pub const fn solarized() -> Theme {
    let base03 = Color::Rgb(0, 43, 54);
    let base02 = Color::Rgb(7, 54, 66);
    let base01 = Color::Rgb(88, 110, 117);
    let base0 = Color::Rgb(131, 148, 150);
    let base1 = Color::Rgb(147, 161, 161);
    let yellow = Color::Rgb(181, 137, 0);
    let orange = Color::Rgb(203, 75, 22);
    let red = Color::Rgb(220, 50, 47);
    let green = Color::Rgb(133, 153, 0);
    let cyan = Color::Rgb(42, 161, 152);
    let blue = Color::Rgb(38, 139, 210);
    let violet = Color::Rgb(108, 113, 196);
    Theme {
        name: "solarized",
        brand: cyan,
        active_tab: yellow,
        inactive_tab: base01,
        border: base01,
        separator: base01,
        text_primary: base0,
        text_secondary: base1,
        text_muted: base01,
        text_inverse: base03,
        status_good: green,
        status_warn: yellow,
        status_error: red,
        status_info: cyan,
        rx_rate: green,
        tx_rate: blue,
        key_hint: orange,
        selection_bg: base02,
        highlight_bg: violet,
        bg: base03,
        warn_bg: Color::Rgb(40, 36, 14),
        err_bg: Color::Rgb(50, 18, 18),
    }
}

pub const fn dracula() -> Theme {
    let bg = Color::Rgb(40, 42, 54);
    let fg = Color::Rgb(248, 248, 242);
    let comment = Color::Rgb(98, 114, 164);
    let cyan = Color::Rgb(139, 233, 253);
    let green = Color::Rgb(80, 250, 123);
    let orange = Color::Rgb(255, 184, 108);
    let pink = Color::Rgb(255, 121, 198);
    let purple = Color::Rgb(189, 147, 249);
    let red = Color::Rgb(255, 85, 85);
    let yellow = Color::Rgb(241, 250, 140);
    Theme {
        name: "dracula",
        brand: purple,
        active_tab: pink,
        inactive_tab: comment,
        border: comment,
        separator: comment,
        text_primary: fg,
        text_secondary: Color::Rgb(200, 200, 210),
        text_muted: comment,
        text_inverse: bg,
        status_good: green,
        status_warn: yellow,
        status_error: red,
        status_info: cyan,
        rx_rate: green,
        tx_rate: cyan,
        key_hint: orange,
        selection_bg: Color::Rgb(68, 71, 90),
        highlight_bg: Color::Rgb(98, 114, 164),
        bg,
        warn_bg: Color::Rgb(60, 50, 30),
        err_bg: Color::Rgb(70, 30, 30),
    }
}

pub const fn nord() -> Theme {
    let polar0 = Color::Rgb(46, 52, 64);
    let polar2 = Color::Rgb(67, 76, 94);
    let snow0 = Color::Rgb(216, 222, 233);
    let snow1 = Color::Rgb(229, 233, 240);
    let frost0 = Color::Rgb(143, 188, 187);
    let frost1 = Color::Rgb(136, 192, 208);
    let frost2 = Color::Rgb(129, 161, 193);
    let frost3 = Color::Rgb(94, 129, 172);
    let aurora_red = Color::Rgb(191, 97, 106);
    let aurora_orange = Color::Rgb(208, 135, 112);
    let aurora_yellow = Color::Rgb(235, 203, 139);
    let aurora_green = Color::Rgb(163, 190, 140);
    Theme {
        name: "nord",
        brand: frost1,
        active_tab: frost0,
        inactive_tab: frost3,
        border: polar2,
        separator: polar2,
        text_primary: snow0,
        text_secondary: snow1,
        text_muted: Color::Rgb(76, 86, 106),
        text_inverse: polar0,
        status_good: aurora_green,
        status_warn: aurora_yellow,
        status_error: aurora_red,
        status_info: frost1,
        rx_rate: aurora_green,
        tx_rate: frost2,
        key_hint: aurora_orange,
        selection_bg: Color::Rgb(59, 66, 82),
        highlight_bg: Color::Rgb(76, 86, 106),
        bg: polar0,
        warn_bg: Color::Rgb(60, 56, 40),
        err_bg: Color::Rgb(60, 36, 38),
    }
}

pub fn by_name(name: &str) -> Theme {
    match name.to_lowercase().as_str() {
        "light" => light(),
        "ocean" => ocean(),
        "solarized" => solarized(),
        "dracula" => dracula(),
        "nord" => nord(),
        _ => dark(),
    }
}

static ACTIVE: RwLock<Theme> = RwLock::new(dark());

pub fn active() -> Theme {
    *ACTIVE.read().expect("theme lock poisoned")
}

pub fn set(theme: Theme) {
    *ACTIVE.write().expect("theme lock poisoned") = theme;
}

pub fn set_by_name(name: &str) {
    set(by_name(name));
}

pub fn name() -> &'static str {
    active().name
}

pub fn cycle() -> &'static str {
    let cur = name();
    let i = THEME_NAMES.iter().position(|n| *n == cur).unwrap_or(0);
    let next = THEME_NAMES[(i + 1) % THEME_NAMES.len()];
    set_by_name(next);
    next
}

#[cfg(test)]
mod tests {
    use super::*;

    fn restore_dark() {
        set_by_name("dark");
    }

    #[test]
    fn all_builtin_themes_load() {
        for name in THEME_NAMES {
            let t = by_name(name);
            assert_eq!(t.name, *name);
        }
    }

    #[test]
    fn unknown_falls_back_to_dark() {
        assert_eq!(by_name("nonsense").name, "dark");
        assert_eq!(by_name("").name, "dark");
    }

    #[test]
    fn cycle_visits_every_theme() {
        restore_dark();
        let mut seen = Vec::new();
        for _ in 0..THEME_NAMES.len() {
            seen.push(cycle());
        }
        assert_eq!(name(), "dark");
        let mut sorted = seen.clone();
        sorted.sort();
        let mut expected: Vec<&str> = THEME_NAMES.to_vec();
        expected.sort();
        assert_eq!(sorted, expected);
        restore_dark();
    }

    #[test]
    fn set_by_name_changes_active() {
        restore_dark();
        set_by_name("dracula");
        assert_eq!(name(), "dracula");
        restore_dark();
    }

    #[test]
    fn dark_preserves_legacy_colors() {
        // Sanity: the original syswatch palette is preserved under the new
        // semantic field names. Guards against accidental color drift.
        let t = dark();
        assert_eq!(t.bg, Color::Rgb(0x0c, 0x14, 0x18));
        assert_eq!(t.text_primary, Color::Rgb(0xc5, 0xd1, 0xd6));
        assert_eq!(t.brand, Color::Rgb(0x5f, 0xdc, 0xff));
        assert_eq!(t.status_good, Color::Rgb(0x5c, 0xd9, 0x89));
        assert_eq!(t.status_warn, Color::Rgb(0xf0, 0xc0, 0x60));
        assert_eq!(t.status_error, Color::Rgb(0xff, 0x78, 0x78));
    }
}
