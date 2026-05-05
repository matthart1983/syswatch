use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{App, Snapshot};
use crate::collect::DiskUsageTick;
use crate::ui::{
    graph::{self, GraphStyle},
    palette as p,
    widgets::{block_bar_styled, human_bytes, human_rate, panel},
};

pub fn draw(f: &mut Frame, area: Rect, app: &App, snap: &Snapshot) {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(7)])
        .split(area);

    draw_devices(f, v[0], snap, app.graph_style);
    draw_throughput(f, v[1], app, snap);
}

fn draw_devices(f: &mut Frame, area: Rect, snap: &Snapshot, style: GraphStyle) {
    // Filter to device-backed mounts only (skip /dev, /proc, tmpfs, etc).
    let devices: Vec<&DiskUsageTick> = snap
        .disks
        .iter()
        .filter(|d| {
            !d.device.is_empty()
                && d.total_bytes > 0
                && (d.device.starts_with("/dev/")
                    || d.fs_type.eq_ignore_ascii_case("apfs")
                    || d.fs_type.eq_ignore_ascii_case("ext4")
                    || d.fs_type.eq_ignore_ascii_case("xfs")
                    || d.fs_type.eq_ignore_ascii_case("btrfs")
                    || d.fs_type.eq_ignore_ascii_case("zfs")
                    || d.fs_type.eq_ignore_ascii_case("ntfs")
                    || d.fs_type.eq_ignore_ascii_case("hfs"))
        })
        .collect();

    let title_right = format!(
        "aggregate {} read  {} write",
        human_rate(snap.disk_io.read_rate),
        human_rate(snap.disk_io.write_rate)
    );
    let block = panel(&format!(
        "BLOCK DEVICES  {}     {}",
        devices.len(),
        title_right
    ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let header = Line::from(vec![
        Span::styled("   ", Style::default().fg(p::text_muted())),
        Span::styled(format!("{:<28} ", "DEVICE"), header_style()),
        Span::styled(format!("{:<32} ", "MOUNT"), header_style()),
        Span::styled(format!("{:<8} ", "FS"), header_style()),
        Span::styled(format!("{:>9} ", "SIZE"), header_style()),
        Span::styled(format!("{:>6} ", "USED"), header_style()),
        Span::styled("USAGE", header_style()),
    ]);

    let mut lines = vec![header];
    let bar_w = inner
        .width
        .saturating_sub(2 + 28 + 1 + 32 + 1 + 8 + 1 + 9 + 1 + 6 + 1);
    for d in devices.iter() {
        let pct = (d.usage_pct / 100.0).clamp(0.0, 1.0);
        let dot_color = bar_color(d.usage_pct);
        let used_color = bar_color(d.usage_pct);
        let bar = block_bar_styled(pct, bar_w, dot_color, style);
        let mut spans = vec![
            Span::styled(" \u{25cf} ", Style::default().fg(dot_color)),
            Span::styled(
                format!("{:<28.28} ", d.device),
                Style::default().fg(p::text_primary()),
            ),
            Span::styled(
                format!("{:<32.32} ", d.mount_point),
                Style::default().fg(p::text_muted()),
            ),
            Span::styled(
                format!("{:<8.8} ", d.fs_type),
                Style::default().fg(p::brand()),
            ),
            Span::styled(
                format!("{:>9} ", human_bytes(d.total_bytes)),
                Style::default().fg(p::text_muted()),
            ),
            Span::styled(
                format!("{:>5.1}% ", d.usage_pct),
                Style::default().fg(used_color),
            ),
        ];
        spans.extend(bar.spans);
        lines.push(Line::from(spans));
    }
    if devices.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "No device-backed mounts detected.",
            Style::default().fg(p::text_muted()),
        )]));
    }
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(p::bg())),
        inner,
    );
}

fn draw_throughput(f: &mut Frame, area: Rect, app: &App, snap: &Snapshot) {
    let block = panel(&format!(
        "THROUGHPUT  all devices  last {}s     read green / write cyan",
        app.history.io_rate.len()
    ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let series: Vec<f32> = app
        .history
        .io_rate
        .to_vec()
        .iter()
        .map(|v| *v as f32)
        .collect();
    let peak = series.iter().cloned().fold(1.0f32, f32::max);
    let normalized: Vec<f32> = series.iter().map(|v| (v / peak).min(1.0)).collect();

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(inner);

    graph::render(f, cols[0], &normalized, app.graph_style, p::brand());

    let counters = vec![
        Line::from(vec![
            Span::styled("read   ", Style::default().fg(p::text_muted())),
            Span::styled(
                human_rate(snap.disk_io.read_rate),
                Style::default()
                    .fg(p::status_good())
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("write  ", Style::default().fg(p::text_muted())),
            Span::styled(
                human_rate(snap.disk_io.write_rate),
                Style::default().fg(p::brand()).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("peak   ", Style::default().fg(p::text_muted())),
            Span::styled(
                human_rate(peak as f64),
                Style::default().fg(p::text_primary()),
            ),
        ]),
        Line::from(vec![
            Span::styled("session", Style::default().fg(p::text_muted())),
            Span::styled(
                format!(
                    " {} read / {} written",
                    human_bytes(snap.disk_io.read_bytes_total),
                    human_bytes(snap.disk_io.write_bytes_total)
                ),
                Style::default().fg(p::text_muted()),
            ),
        ]),
    ];
    f.render_widget(
        Paragraph::new(counters).style(Style::default().bg(p::bg())),
        cols[1],
    );
}

fn bar_color(used_pct: f32) -> ratatui::style::Color {
    if used_pct >= 90.0 {
        p::status_error()
    } else if used_pct >= 70.0 {
        p::status_warn()
    } else {
        p::status_good()
    }
}

fn header_style() -> Style {
    Style::default()
        .fg(p::text_muted())
        .add_modifier(Modifier::BOLD)
}
