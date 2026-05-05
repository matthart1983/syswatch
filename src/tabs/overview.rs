use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Cell, Paragraph, Row, Table},
    Frame,
};

use crate::app::{App, Snapshot};
use crate::insights::Severity;
use crate::ui::{
    graph::{self, GraphStyle},
    palette as p,
    widgets::{block_bar_styled, human_bytes, human_rate, panel},
};

pub fn draw(f: &mut Frame, area: Rect, app: &App, snap: &Snapshot) {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7), // KPI strip
            Constraint::Min(0),    // body
            Constraint::Length(6), // insights strip
        ])
        .split(area);

    draw_kpi_strip(f, v[0], app, snap);
    draw_middle(f, v[1], app, snap);
    draw_insights_strip(f, v[2], app);
}

fn draw_kpi_strip(f: &mut Frame, area: Rect, app: &App, snap: &Snapshot) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Ratio(1, 5),
            Constraint::Ratio(1, 5),
            Constraint::Ratio(1, 5),
            Constraint::Ratio(1, 5),
            Constraint::Ratio(1, 5),
        ])
        .split(area);

    let style = app.graph_style;
    kpi_tile(
        f,
        cols[0],
        "CPU",
        format!("{:.0}%", snap.cpu.usage_pct),
        kpi_color(snap.cpu.usage_pct, 60.0, 85.0),
        &app.history.cpu.to_vec(),
        100.0,
        style,
    );
    let mem_pct = if snap.mem.total_bytes > 0 {
        100.0 * snap.mem.used_bytes as f32 / snap.mem.total_bytes as f32
    } else {
        0.0
    };
    kpi_tile(
        f,
        cols[1],
        "MEM",
        format!("{:.0}%", mem_pct),
        kpi_color(mem_pct, 70.0, 90.0),
        &app.history
            .mem
            .to_vec()
            .iter()
            .map(|v| v * 100.0)
            .collect::<Vec<f32>>(),
        100.0,
        style,
    );
    let swap_pct = if snap.mem.swap_total_bytes > 0 {
        100.0 * snap.mem.swap_used_bytes as f32 / snap.mem.swap_total_bytes as f32
    } else {
        0.0
    };
    kpi_tile(
        f,
        cols[2],
        "SWAP",
        format!("{:.0}%", swap_pct),
        kpi_color(swap_pct, 25.0, 75.0),
        &[],
        100.0,
        style,
    );
    let io = snap.disk_io.read_rate + snap.disk_io.write_rate;
    kpi_tile(
        f,
        cols[3],
        "DISK IO",
        human_rate(io),
        p::brand(),
        &app.history
            .io_rate
            .to_vec()
            .iter()
            .map(|v| *v as f32)
            .collect::<Vec<f32>>(),
        max_or(&app.history.io_rate.to_vec(), 1.0) as f32,
        style,
    );
    let net: f64 = snap.net.iter().map(|i| i.rx_rate + i.tx_rate).sum();
    kpi_tile(
        f,
        cols[4],
        "NET",
        human_rate(net),
        p::brand(),
        &app.history
            .net_rate
            .to_vec()
            .iter()
            .map(|v| *v as f32)
            .collect::<Vec<f32>>(),
        max_or(&app.history.net_rate.to_vec(), 1.0) as f32,
        style,
    );
}

fn kpi_tile(
    f: &mut Frame,
    area: Rect,
    label: &str,
    value: String,
    accent: ratatui::style::Color,
    series: &[f32],
    series_max: f32,
    style: GraphStyle,
) {
    let block = panel(label);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let h = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(inner);

    let val = Paragraph::new(Line::from(vec![Span::styled(
        value,
        Style::default().fg(accent).add_modifier(Modifier::BOLD),
    )]))
    .style(Style::default().bg(p::bg()));
    f.render_widget(val, h[0]);

    let bar = block_bar_styled(
        if series_max > 0.0 {
            series.last().copied().unwrap_or(0.0) / series_max
        } else {
            0.0
        },
        h[1].width,
        accent,
        style,
    );
    f.render_widget(Paragraph::new(bar).style(Style::default().bg(p::bg())), h[1]);

    if !series.is_empty() && series_max > 0.0 {
        let normalized: Vec<f32> = series.iter().map(|v| (v / series_max).min(1.0)).collect();
        graph::render(f, h[2], &normalized, style, accent);
    }
}

fn draw_middle(f: &mut Frame, area: Rect, app: &App, snap: &Snapshot) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(area);

    draw_per_core(f, cols[0], snap, app.graph_style);
    draw_top_procs(f, cols[1], snap);
}

fn draw_per_core(f: &mut Frame, area: Rect, snap: &Snapshot, style: GraphStyle) {
    let block = panel("Per-core CPU");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    for (i, pct) in snap.cpu.per_core.iter().enumerate() {
        let color = kpi_color(*pct, 60.0, 85.0);
        let bar = block_bar_styled(*pct / 100.0, inner.width.saturating_sub(12), color, style);
        let mut spans = vec![Span::styled(
            format!("c{:>2} ", i),
            Style::default().fg(p::text_muted()),
        )];
        spans.extend(bar.spans);
        spans.push(Span::styled(
            format!(" {:>3.0}%", pct),
            Style::default().fg(p::text_primary()),
        ));
        lines.push(Line::from(spans));
    }
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(p::bg())),
        inner,
    );
}

fn draw_top_procs(f: &mut Frame, area: Rect, snap: &Snapshot) {
    let block = panel("Top processes (by CPU)");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let header = Row::new(vec![
        Cell::from("PID"),
        Cell::from("USER"),
        Cell::from("%CPU"),
        Cell::from("RSS"),
        Cell::from("COMMAND"),
    ])
    .style(Style::default().fg(p::text_muted()).add_modifier(Modifier::BOLD));

    let rows = snap
        .procs
        .iter()
        .take(inner.height.saturating_sub(1) as usize)
        .map(|p_| {
            Row::new(vec![
                Cell::from(p_.pid.to_string()),
                Cell::from(p_.user.clone()),
                Cell::from(format!("{:.1}", p_.cpu_pct))
                    .style(Style::default().fg(kpi_color(p_.cpu_pct, 30.0, 70.0))),
                Cell::from(human_bytes(p_.mem_rss)),
                Cell::from(p_.name.clone()),
            ])
        });

    let widths = [
        Constraint::Length(7),
        Constraint::Length(10),
        Constraint::Length(6),
        Constraint::Length(10),
        Constraint::Min(0),
    ];
    // Cells without an explicit fg inherit this style's fg. Without setting
    // it, cells render with `Color::Reset` (terminal default fg), which on a
    // dark-terminal user with the light theme picks the wrong color.
    let table = Table::new(rows, widths)
        .header(header)
        .style(Style::default().fg(p::text_primary()).bg(p::bg()));
    f.render_widget(table, inner);
}

fn draw_insights_strip(f: &mut Frame, area: Rect, app: &App) {
    let active = app
        .insights
        .iter()
        .filter(|i| i.severity != Severity::Info)
        .count();
    let title = if active == 0 {
        "Insights — system nominal".to_string()
    } else {
        format!("Insights — {} active (press [+] for detail)", active)
    };
    let block = panel(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.insights.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                "No anomalies in CPU, Memory, Disks, Procs, Net.",
                Style::default().fg(p::text_muted()),
            )]))
            .style(Style::default().bg(p::bg())),
            inner,
        );
        return;
    }

    let take = inner.height as usize;
    let lines: Vec<Line> = app
        .insights
        .iter()
        .take(take)
        .map(|ins| {
            let (color, label) = match ins.severity {
                Severity::Crit => (p::status_error(), "CRIT"),
                Severity::Warn => (p::status_warn(), "WARN"),
                Severity::Info => (p::brand(), "INFO"),
            };
            Line::from(vec![
                Span::styled(
                    format!(" {} ", label),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(ins.title.clone(), Style::default().fg(p::text_primary())),
                Span::styled(
                    format!("   \u{2192} {}", ins.suggested_tab.title()),
                    Style::default().fg(p::text_muted()),
                ),
            ])
        })
        .collect();

    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(p::bg())),
        inner,
    );
}

fn kpi_color(v: f32, warn: f32, crit: f32) -> ratatui::style::Color {
    if v >= crit {
        p::status_error()
    } else if v >= warn {
        p::status_warn()
    } else {
        p::status_good()
    }
}

fn max_or(xs: &[f64], min: f64) -> f64 {
    xs.iter().cloned().fold(min, f64::max)
}
