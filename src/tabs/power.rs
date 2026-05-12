use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{App, Snapshot};
use crate::collect::{BatteryTick, PowerSource, PowerTick};
use crate::ui::{
    graph::GraphStyle,
    palette as p,
    widgets::{block_bar_styled, panel},
};

pub fn draw(f: &mut Frame, area: Rect, app: &App, snap: &Snapshot) {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(8), // battery
            Constraint::Length(7), // power source / throttle / draw
            Constraint::Min(0),    // thermal zones / fans
        ])
        .split(area);

    draw_battery(f, v[0], &snap.power, app.graph_style);
    draw_status(f, v[1], &snap.power);
    draw_thermal(f, v[2], &snap.power, app.graph_style);
}

fn draw_battery(f: &mut Frame, area: Rect, pwr: &PowerTick, style: GraphStyle) {
    let block = panel("BATTERY");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(bat) = &pwr.battery else {
        f.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                "No battery detected (desktop or VM).",
                Style::default().fg(p::text_muted()),
            )]))
            .style(Style::default().bg(p::bg())),
            inner,
        );
        return;
    };

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(inner);

    // Big bar.
    let pct = (bat.charge_pct / 100.0).clamp(0.0, 1.0);
    let color = charge_color(bat.charge_pct, bat.is_charging);
    let header_text = state_text(bat);
    let bar_lines = vec![
        Line::from(vec![Span::styled(
            format!("{:>5.0}%", bat.charge_pct),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )]),
        Line::from(""),
        block_bar_styled(pct, cols[0].width.saturating_sub(2), color, style),
        Line::from(""),
        Line::from(vec![Span::styled(
            header_text,
            Style::default().fg(p::text_muted()),
        )]),
    ];
    f.render_widget(
        Paragraph::new(bar_lines).style(Style::default().bg(p::bg())),
        cols[0],
    );

    // Side stats.
    let mut lines: Vec<Line> = Vec::new();
    if let Some(t) = bat.time_remaining_min {
        let h = t / 60;
        let m = t % 60;
        let label = if bat.is_charging {
            "to full"
        } else {
            "remaining"
        };
        lines.push(kv(label, format!("{}:{:02}", h, m), p::text_primary()));
    } else {
        lines.push(kv("remaining", "calculating…".into(), p::text_muted()));
    }
    if let Some(c) = bat.cycle_count {
        lines.push(kv("cycles", c.to_string(), p::text_primary()));
    }
    if let Some(h) = bat.health_pct {
        let color = if h >= 80.0 {
            p::status_good()
        } else if h >= 60.0 {
            p::status_warn()
        } else {
            p::status_error()
        };
        lines.push(kv("health", format!("{:.0}%", h), color));
    }
    if let Some(t) = bat.temp_c {
        let color = if t >= 40.0 {
            p::status_error()
        } else if t >= 35.0 {
            p::status_warn()
        } else {
            p::status_good()
        };
        lines.push(kv("temp", format!("{:.1}°C", t), color));
    }
    if let Some(v) = bat.voltage_v {
        lines.push(kv("voltage", format!("{:.2} V", v), p::text_muted()));
    }
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(p::bg())),
        cols[1],
    );
}

fn draw_status(f: &mut Frame, area: Rect, pwr: &PowerTick) {
    let block = panel("POWER STATUS");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(33),
            Constraint::Percentage(33),
            Constraint::Percentage(34),
        ])
        .split(inner);

    // Source.
    let (src_color, src_glyph) = match pwr.source {
        PowerSource::Ac => (p::status_good(), "\u{26A1}"),
        PowerSource::Battery => (p::status_warn(), "\u{1F50B}"),
        PowerSource::Unknown => (p::text_muted(), "?"),
    };
    f.render_widget(
        Paragraph::new(vec![
            Line::from(vec![Span::styled(
                "source",
                Style::default().fg(p::text_muted()),
            )]),
            Line::from(""),
            Line::from(vec![
                Span::styled(format!("{} ", src_glyph), Style::default().fg(src_color)),
                Span::styled(
                    pwr.source.label(),
                    Style::default().fg(src_color).add_modifier(Modifier::BOLD),
                ),
            ]),
        ])
        .style(Style::default().bg(p::bg())),
        cols[0],
    );

    // Throttle.
    let (throttle_color, throttle_text, throttle_detail): (ratatui::style::Color, String, String) =
        match pwr.thermal_throttle_pct {
            Some(100) => (
                p::status_good(),
                "no throttle".into(),
                "CPU at 100% nominal speed".into(),
            ),
            Some(n) if n >= 80 => (
                p::status_warn(),
                format!("{}%", n),
                "thermal throttling — mild".into(),
            ),
            Some(n) => (
                p::status_error(),
                format!("{}%", n),
                "thermal throttling — significant".into(),
            ),
            None => (
                p::text_muted(),
                "—".into(),
                "platform doesn't expose throttle state".into(),
            ),
        };
    f.render_widget(
        Paragraph::new(vec![
            Line::from(vec![Span::styled(
                "thermal",
                Style::default().fg(p::text_muted()),
            )]),
            Line::from(""),
            Line::from(vec![Span::styled(
                throttle_text,
                Style::default()
                    .fg(throttle_color)
                    .add_modifier(Modifier::BOLD),
            )]),
            Line::from(vec![Span::styled(
                throttle_detail,
                Style::default().fg(p::text_muted()),
            )]),
        ])
        .style(Style::default().bg(p::bg())),
        cols[1],
    );

    // Power draw.
    let (draw_color, draw_text, draw_detail): (ratatui::style::Color, String, String) =
        match pwr.system_power_w {
            Some(w) => (
                if w >= 30.0 {
                    p::status_error()
                } else if w >= 15.0 {
                    p::status_warn()
                } else {
                    p::status_good()
                },
                format!("{:.1} W", w),
                "system draw at the battery".into(),
            ),
            None => (
                p::text_muted(),
                "—".into(),
                "needs sudo powermetrics on macOS".into(),
            ),
        };
    f.render_widget(
        Paragraph::new(vec![
            Line::from(vec![Span::styled(
                "draw",
                Style::default().fg(p::text_muted()),
            )]),
            Line::from(""),
            Line::from(vec![Span::styled(
                draw_text,
                Style::default().fg(draw_color).add_modifier(Modifier::BOLD),
            )]),
            Line::from(vec![Span::styled(
                draw_detail,
                Style::default().fg(p::text_muted()),
            )]),
        ])
        .style(Style::default().bg(p::bg())),
        cols[2],
    );
}

fn draw_thermal(f: &mut Frame, area: Rect, pwr: &PowerTick, style: GraphStyle) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area);

    // Thermal zones.
    let zones_block = panel(format!("THERMAL ZONES  {}", pwr.thermal_zones.len()));
    let zones_inner = zones_block.inner(cols[0]);
    f.render_widget(zones_block, cols[0]);

    if pwr.thermal_zones.is_empty() {
        let hint = pwr
            .live_data_hint
            .clone()
            .unwrap_or_else(|| "platform doesn't expose thermal zones without sudo.".into());
        f.render_widget(
            Paragraph::new(vec![
                Line::from(vec![Span::styled(
                    "no zones reported",
                    Style::default().fg(p::text_muted()),
                )]),
                Line::from(""),
                Line::from(vec![Span::styled(hint, Style::default().fg(p::border()))]),
            ])
            .style(Style::default().bg(p::bg())),
            zones_inner,
        );
    } else {
        let mut lines = Vec::new();
        for z in pwr.thermal_zones.iter() {
            let color = if z.temp_c >= 80.0 {
                p::status_error()
            } else if z.temp_c >= 60.0 {
                p::status_warn()
            } else {
                p::status_good()
            };
            let bar = block_bar_styled(
                (z.temp_c / 100.0).clamp(0.0, 1.0),
                zones_inner.width.saturating_sub(28),
                color,
                style,
            );
            let mut spans = vec![
                Span::styled(
                    format!("{:<18.18} ", z.name),
                    Style::default().fg(p::text_primary()),
                ),
                Span::styled(format!("{:>5.1}°C ", z.temp_c), Style::default().fg(color)),
            ];
            spans.extend(bar.spans);
            lines.push(Line::from(spans));
        }
        f.render_widget(
            Paragraph::new(lines).style(Style::default().bg(p::bg())),
            zones_inner,
        );
    }

    // Fans.
    let fans_block = panel(format!("FANS  {}", pwr.fans.len()));
    let fans_inner = fans_block.inner(cols[1]);
    f.render_widget(fans_block, cols[1]);

    if pwr.fans.is_empty() {
        let hint = pwr.live_data_hint.clone().unwrap_or_else(|| {
            "no fans reported (passive cooling or platform doesn't expose them).".into()
        });
        f.render_widget(
            Paragraph::new(vec![
                Line::from(vec![Span::styled(
                    "no fan data",
                    Style::default().fg(p::text_muted()),
                )]),
                Line::from(""),
                Line::from(vec![Span::styled(hint, Style::default().fg(p::border()))]),
            ])
            .style(Style::default().bg(p::bg())),
            fans_inner,
        );
    } else {
        let lines: Vec<Line> = pwr
            .fans
            .iter()
            .map(|fan| {
                Line::from(vec![
                    Span::styled(
                        format!("{:<10.10} ", fan.name),
                        Style::default().fg(p::text_primary()),
                    ),
                    Span::styled(
                        format!("{:>5} RPM", fan.rpm),
                        Style::default().fg(p::brand()),
                    ),
                    Span::styled(
                        match fan.target_rpm {
                            Some(t) => format!("  → {} target", t),
                            None => String::new(),
                        },
                        Style::default().fg(p::text_muted()),
                    ),
                ])
            })
            .collect();
        f.render_widget(
            Paragraph::new(lines).style(Style::default().bg(p::bg())),
            fans_inner,
        );
    }
}

fn state_text(bat: &BatteryTick) -> String {
    if bat.fully_charged {
        "fully charged".into()
    } else if bat.is_charging {
        format!(
            "charging{}",
            bat.amperage_ma
                .map(|a| format!(" @ {:.1} A", a as f32 / 1000.0))
                .unwrap_or_default()
        )
    } else {
        format!(
            "discharging{}",
            bat.amperage_ma
                .map(|a| format!(" @ {:.1} A", a.unsigned_abs() as f32 / 1000.0))
                .unwrap_or_default()
        )
    }
}

fn charge_color(pct: f32, is_charging: bool) -> ratatui::style::Color {
    if is_charging {
        p::brand()
    } else if pct <= 15.0 {
        p::status_error()
    } else if pct <= 30.0 {
        p::status_warn()
    } else {
        p::status_good()
    }
}

fn kv(k: &str, v: String, val_color: ratatui::style::Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{:<10} ", k), Style::default().fg(p::text_muted())),
        Span::styled(v, Style::default().fg(val_color)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_text_fully_charged_wins_over_charging_flag() {
        let b = BatteryTick {
            fully_charged: true,
            is_charging: true,
            amperage_ma: Some(1500),
            ..BatteryTick::default()
        };
        assert_eq!(state_text(&b), "fully charged");
    }

    #[test]
    fn state_text_charging_with_amperage_formats_amps() {
        let b = BatteryTick {
            is_charging: true,
            amperage_ma: Some(1500),
            ..BatteryTick::default()
        };
        assert_eq!(state_text(&b), "charging @ 1.5 A");
    }

    #[test]
    fn state_text_charging_without_amperage_drops_suffix() {
        let b = BatteryTick {
            is_charging: true,
            amperage_ma: None,
            ..BatteryTick::default()
        };
        assert_eq!(state_text(&b), "charging");
    }

    #[test]
    fn state_text_discharging_renders_amperage_magnitude() {
        // Discharging reports negative amperage; the user-facing string
        // should show the magnitude, not the sign.
        let b = BatteryTick {
            is_charging: false,
            amperage_ma: Some(-2000),
            ..BatteryTick::default()
        };
        assert_eq!(state_text(&b), "discharging @ 2.0 A");
    }

    #[test]
    fn state_text_discharging_without_amperage() {
        let b = BatteryTick::default();
        assert_eq!(state_text(&b), "discharging");
    }

    #[test]
    fn charge_color_charging_uses_brand_regardless_of_pct() {
        // A 5% battery on AC shouldn't render red.
        assert_eq!(charge_color(5.0, true), p::brand());
        assert_eq!(charge_color(95.0, true), p::brand());
    }

    #[test]
    fn charge_color_discharging_thresholds() {
        assert_eq!(charge_color(0.0, false), p::status_error());
        assert_eq!(charge_color(15.0, false), p::status_error());
        assert_eq!(charge_color(15.5, false), p::status_warn());
        assert_eq!(charge_color(30.0, false), p::status_warn());
        assert_eq!(charge_color(30.5, false), p::status_good());
        assert_eq!(charge_color(100.0, false), p::status_good());
    }

    // ── TestBackend rendering tests ─────────────────────────
    //
    // Exercise the ratatui draw pipeline against an in-memory backend
    // and assert on the resulting buffer text. Catches layout/content
    // regressions that pure-helper tests can't see. Pattern is portable:
    // any tab's private draw_* fn that takes a tick + style can be
    // tested this way without constructing a full App.

    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn buffer_to_string(buf: &ratatui::buffer::Buffer) -> String {
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn draw_battery_renders_fallback_when_battery_absent() {
        // No battery (desktop / VM) — must surface a user-readable
        // explanation, not just an empty panel.
        let pwr = PowerTick::default();
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_battery(f, f.area(), &pwr, GraphStyle::Bars);
            })
            .unwrap();

        let text = buffer_to_string(terminal.backend().buffer());
        assert!(text.contains("BATTERY"), "missing panel title:\n{text}");
        assert!(
            text.contains("No battery detected"),
            "missing fallback line:\n{text}"
        );
    }

    #[test]
    fn draw_battery_renders_percentage_and_charging_state() {
        // Charging at 73% with 1.5A draw — the panel should surface
        // the percentage, the "charging" state, and the amperage suffix.
        let pwr = PowerTick {
            battery: Some(BatteryTick {
                charge_pct: 73.0,
                is_charging: true,
                amperage_ma: Some(1500),
                ..BatteryTick::default()
            }),
            ..PowerTick::default()
        };
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_battery(f, f.area(), &pwr, GraphStyle::Bars);
            })
            .unwrap();

        let text = buffer_to_string(terminal.backend().buffer());
        assert!(text.contains("73%"), "missing percentage:\n{text}");
        assert!(text.contains("charging"), "missing state:\n{text}");
        assert!(text.contains("1.5 A"), "missing amperage suffix:\n{text}");
    }

    #[test]
    fn draw_status_renders_throttle_text_for_known_state() {
        let pwr = PowerTick {
            source: PowerSource::Battery,
            thermal_throttle_pct: Some(100),
            ..PowerTick::default()
        };
        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_status(f, f.area(), &pwr);
            })
            .unwrap();

        let text = buffer_to_string(terminal.backend().buffer());
        assert!(
            text.contains("POWER STATUS"),
            "missing panel title:\n{text}"
        );
        assert!(text.contains("Battery"), "missing source label:\n{text}");
        assert!(
            text.contains("no throttle"),
            "missing throttle label:\n{text}"
        );
    }
}
