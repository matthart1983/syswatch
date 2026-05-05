use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{App, Snapshot};
use crate::ui::{
    palette as p,
    widgets::{block_bar_styled, human_bytes, panel},
};

pub fn draw(f: &mut Frame, area: Rect, app: &App, snap: &Snapshot) {
    let healthy = snap.disks.iter().filter(|d| d.usage_pct < 70.0).count();
    let warn = snap
        .disks
        .iter()
        .filter(|d| d.usage_pct >= 70.0 && d.usage_pct < 90.0)
        .count();
    let crit = snap.disks.iter().filter(|d| d.usage_pct >= 90.0).count();

    let title = format!(
        "MOUNTS  {}     {} healthy  {} nearing full  {} critical",
        snap.disks.len(),
        healthy,
        warn,
        crit
    );
    let block = panel(&title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Column widths.
    let mount_w = 28u16;
    let dev_w = 18u16;
    let fs_w = 8u16;
    let size_w = 9u16;
    let used_w = 7u16;
    let fixed = 3 + mount_w + 1 + dev_w + 1 + fs_w + 1 + size_w + 1 + used_w + 1;
    let bar_w = inner.width.saturating_sub(fixed);

    let header = Line::from(vec![
        Span::styled("   ", Style::default().fg(p::text_muted())),
        Span::styled(
            format!("{:<w$} ", "MOUNT POINT", w = mount_w as usize),
            header_style(),
        ),
        Span::styled(
            format!("{:<w$} ", "DEVICE", w = dev_w as usize),
            header_style(),
        ),
        Span::styled(format!("{:<w$} ", "FS", w = fs_w as usize), header_style()),
        Span::styled(
            format!("{:>w$} ", "SIZE", w = size_w as usize),
            header_style(),
        ),
        Span::styled(
            format!("{:>w$} ", "USED", w = used_w as usize),
            header_style(),
        ),
        Span::styled("USAGE", header_style()),
    ]);

    let mut lines = vec![header];
    let mut sorted = snap.disks.clone();
    sorted.sort_by(|a, b| {
        b.usage_pct
            .partial_cmp(&a.usage_pct)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let take = inner.height.saturating_sub(1) as usize;
    for d in sorted.iter().take(take) {
        let pct = (d.usage_pct / 100.0).clamp(0.0, 1.0);
        let color = bar_color(d.usage_pct);
        let bar = block_bar_styled(pct, bar_w, color, app.graph_style);
        let mut spans = vec![
            Span::styled(" \u{25cf} ", Style::default().fg(color)),
            Span::styled(
                format!("{:<w$.w$} ", d.mount_point, w = mount_w as usize),
                Style::default().fg(p::text_primary()),
            ),
            Span::styled(
                format!("{:<w$.w$} ", d.device, w = dev_w as usize),
                Style::default().fg(p::text_muted()),
            ),
            Span::styled(
                format!("{:<w$.w$} ", d.fs_type, w = fs_w as usize),
                Style::default().fg(p::brand()),
            ),
            Span::styled(
                format!("{:>w$} ", human_bytes(d.total_bytes), w = size_w as usize),
                Style::default().fg(p::text_muted()),
            ),
            Span::styled(
                format!(
                    "{:>w$} ",
                    format!("{:.1}%", d.usage_pct),
                    w = used_w as usize
                ),
                Style::default().fg(color),
            ),
        ];
        spans.extend(bar.spans);
        lines.push(Line::from(spans));
    }
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(p::bg())),
        inner,
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
    Style::default().fg(p::text_muted()).add_modifier(Modifier::BOLD)
}
