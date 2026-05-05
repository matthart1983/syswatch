//! Color accessors for the syswatch UI.
//!
//! Field names mirror netwatch's theme conventions. Reads route to the
//! active theme (`crate::ui::theme`) so swapping themes recolors the
//! whole UI on the next draw.
//!
//! Some accessors (`separator`, `text_secondary`, `text_inverse`, `rx_rate`,
//! `highlight_bg`) aren't read by the current UI — they're kept on purpose
//! so the palette API matches netwatch's slot set 1:1.

#![allow(dead_code)]

use ratatui::style::Color;

use crate::ui::theme;

// ── Brand / chrome ─────────────────────────────────────────
pub fn brand() -> Color {
    theme::active().brand
}
pub fn active_tab() -> Color {
    theme::active().active_tab
}
pub fn inactive_tab() -> Color {
    theme::active().inactive_tab
}
pub fn border() -> Color {
    theme::active().border
}
pub fn separator() -> Color {
    theme::active().separator
}

// ── Text ───────────────────────────────────────────────────
pub fn text_primary() -> Color {
    theme::active().text_primary
}
pub fn text_secondary() -> Color {
    theme::active().text_secondary
}
pub fn text_muted() -> Color {
    theme::active().text_muted
}
pub fn text_inverse() -> Color {
    theme::active().text_inverse
}

// ── Status ────────────────────────────────────────────────
pub fn status_good() -> Color {
    theme::active().status_good
}
pub fn status_warn() -> Color {
    theme::active().status_warn
}
pub fn status_error() -> Color {
    theme::active().status_error
}
pub fn status_info() -> Color {
    theme::active().status_info
}

// ── Data ───────────────────────────────────────────────────
pub fn rx_rate() -> Color {
    theme::active().rx_rate
}
pub fn tx_rate() -> Color {
    theme::active().tx_rate
}
pub fn key_hint() -> Color {
    theme::active().key_hint
}

// ── Selection ─────────────────────────────────────────────
pub fn selection_bg() -> Color {
    theme::active().selection_bg
}
pub fn highlight_bg() -> Color {
    theme::active().highlight_bg
}

// ── Surfaces (syswatch extensions) ───────────────────────
pub fn bg() -> Color {
    theme::active().bg
}
pub fn warn_bg() -> Color {
    theme::active().warn_bg
}
pub fn err_bg() -> Color {
    theme::active().err_bg
}
