use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{App, Snapshot};
use crate::insights::Severity;
use crate::ui::{
    palette as p,
    widgets::{panel, sparkline},
};

pub fn draw(f: &mut Frame, area: Rect, app: &App, _snap: &Snapshot) {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(10), // activity strips
            Constraint::Min(0),     // event log
            Constraint::Length(4),  // scrubber
        ])
        .split(area);

    draw_activity(f, v[0], app);
    draw_events(f, v[1], app);
    draw_scrubber(f, v[2], app);
}

fn draw_activity(f: &mut Frame, area: Rect, app: &App) {
    let block = panel("ACTIVITY  last session window");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let take = inner.width as usize;
    let cpu = window_normalized(&app.history.cpu.to_vec(), take, 100.0);
    let mem = window_normalized(
        &app.history
            .mem
            .to_vec()
            .iter()
            .map(|v| v * 100.0)
            .collect::<Vec<_>>(),
        take,
        100.0,
    );
    let io_raw: Vec<f32> = app
        .history
        .io_rate
        .to_vec()
        .iter()
        .map(|v| *v as f32)
        .collect();
    let io_peak = io_raw.iter().cloned().fold(1.0f32, f32::max);
    let io = window_normalized(&io_raw, take, io_peak);
    let net_raw: Vec<f32> = app
        .history
        .net_rate
        .to_vec()
        .iter()
        .map(|v| *v as f32)
        .collect();
    let net_peak = net_raw.iter().cloned().fold(1.0f32, f32::max);
    let net = window_normalized(&net_raw, take, net_peak);

    let strips = [
        ("cpu  ", &cpu, p::status_good()),
        ("mem  ", &mem, p::status_warn()),
        ("io   ", &io, p::brand()),
        ("net  ", &net, p::tx_rate()),
    ];

    let mut lines: Vec<Line> = Vec::new();
    for (label, series, color) in strips.iter() {
        let mut spans: Vec<Span> = vec![Span::styled(
            label.to_string(),
            Style::default().fg(p::text_muted()),
        )];
        let line = sparkline(series, *color);
        spans.extend(line.spans);
        // Highlight the scrub cursor inside the strip if applicable.
        if app.scrub_offset > 0 {
            mark_cursor(&mut spans, series.len(), app.scrub_offset, *color);
        }
        lines.push(Line::from(spans));
        lines.push(Line::from(""));
    }
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(p::bg())),
        inner,
    );
}

/// Replace the cursor cell with a half-block ▌ and a contrasting color so the
/// user can see exactly which tick they're inspecting.
fn mark_cursor(
    spans: &mut [Span<'static>],
    series_len: usize,
    scrub: usize,
    _accent: ratatui::style::Color,
) {
    if series_len == 0 || scrub >= series_len {
        return;
    }
    let cursor_idx = series_len - 1 - scrub;
    // The sparkline span is the second span (after the label). Replace one char.
    if let Some(spark_span) = spans.get_mut(1) {
        let mut chars: Vec<char> = spark_span.content.chars().collect();
        if cursor_idx < chars.len() {
            chars[cursor_idx] = '\u{2503}'; // ┃
        }
        let new_content: String = chars.into_iter().collect();
        *spark_span = Span::styled(
            new_content,
            spark_span.style.fg(p::brand()).add_modifier(Modifier::BOLD),
        );
    }
}

fn draw_events(f: &mut Frame, area: Rect, app: &App) {
    let block = panel("EVENTS  derived from session");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let events = derive_events(app);
    if events.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                "No events yet — events appear when insights begin/clear or top procs change.",
                Style::default().fg(p::text_muted()),
            )]))
            .style(Style::default().bg(p::bg())),
            inner,
        );
        return;
    }

    let header = Line::from(vec![
        Span::styled(format!("{:>8} ", "T-"), header_style()),
        Span::styled(format!("{:<5} ", "KIND"), header_style()),
        Span::styled("DETAIL", header_style()),
    ]);
    let mut lines = vec![header];
    let take = inner.height.saturating_sub(1) as usize;
    for ev in events.iter().take(take) {
        let color = match ev.kind {
            EventKind::InsightStart => p::status_warn(),
            EventKind::InsightClear => p::status_good(),
            EventKind::TopProcChange => p::brand(),
        };
        let kind_label = match ev.kind {
            EventKind::InsightStart => "WARN",
            EventKind::InsightClear => "OK  ",
            EventKind::TopProcChange => "PROC",
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:>7}s ", ev.age_secs),
                Style::default().fg(p::text_muted()),
            ),
            Span::styled(
                format!("{:<5} ", kind_label),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(ev.detail.clone(), Style::default().fg(p::text_primary())),
        ]));
    }
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(p::bg())),
        inner,
    );
}

fn draw_scrubber(f: &mut Frame, area: Rect, app: &App) {
    let block = panel("SCRUBBER  ←/→ step  Home oldest  End live");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let len = app.history.session.len();
    let bar_w = inner.width as usize;
    let pos = if len <= 1 {
        bar_w.saturating_sub(1)
    } else {
        let live_x = bar_w.saturating_sub(1);
        let frac = 1.0 - (app.scrub_offset as f32 / (len - 1) as f32);
        ((live_x as f32) * frac).round() as usize
    };

    let mut line_chars: Vec<(char, ratatui::style::Color, bool)> = (0..bar_w)
        .map(|_| ('\u{2500}', p::border(), false))
        .collect();
    if pos < line_chars.len() {
        line_chars[pos] = ('\u{25CF}', p::brand(), true);
    }
    // Mark "now" at the right edge if not the cursor.
    if let Some(last) = line_chars.last_mut() {
        if !last.2 {
            *last = ('\u{2502}', p::text_muted(), false);
        }
    }
    // Mark "oldest" at the left edge if not the cursor.
    if let Some(first) = line_chars.first_mut() {
        if !first.2 && len > 1 {
            *first = ('\u{2502}', p::text_muted(), false);
        }
    }

    let bar_line = Line::from(
        line_chars
            .iter()
            .map(|(c, color, bold)| {
                let mut style = Style::default().fg(*color);
                if *bold {
                    style = style.add_modifier(Modifier::BOLD);
                }
                Span::styled(c.to_string(), style)
            })
            .collect::<Vec<_>>(),
    );

    let scrubbed = app.displayed_snap();
    let (status, status_color) = match (app.scrub_offset, scrubbed) {
        (0, _) => ("LIVE  showing newest tick".into(), p::status_good()),
        (_, Some(snap)) => {
            let ts: chrono::DateTime<chrono::Local> = snap.t.into();
            (
                format!(
                    "SCRUB  -{}s  ({} of {} ticks back)",
                    app.scrub_offset,
                    app.scrub_offset,
                    len.saturating_sub(1)
                ) + &format!("    {}", ts.format("%H:%M:%S")),
                p::brand(),
            )
        }
        _ => ("(no session yet)".into(), p::text_muted()),
    };

    let info_line = Line::from(vec![Span::styled(
        status,
        Style::default()
            .fg(status_color)
            .add_modifier(Modifier::BOLD),
    )]);

    f.render_widget(
        Paragraph::new(vec![bar_line, info_line]).style(Style::default().bg(p::bg())),
        inner,
    );
}

#[derive(Debug, Clone, Copy)]
enum EventKind {
    InsightStart,
    /// Reserved for future use. The `derive_events` walk would emit this
    /// when an insight clears across two consecutive snapshots, but
    /// recomputing insights per-tick during scrub is too expensive today.
    /// `draw_events` already has the render arm wired up.
    #[allow(dead_code)]
    InsightClear,
    TopProcChange,
}

#[derive(Debug, Clone)]
struct Event {
    kind: EventKind,
    age_secs: u64,
    detail: String,
}

fn derive_events(app: &App) -> Vec<Event> {
    // Walk the session, comparing adjacent snapshots for top-proc changes.
    // Insight transitions need recomputing the insight set per tick — too
    // expensive for now, so we surface only the *current* active insights and
    // top-proc churn from the session.
    let session: Vec<Snapshot> = app.history.session.to_vec();
    if session.len() < 2 {
        return Vec::new();
    }
    let now = session
        .last()
        .map(|s| s.t)
        .unwrap_or(std::time::SystemTime::now());

    let mut out: Vec<Event> = Vec::new();

    // Top-proc changes: when the #1 by CPU changes from one tick to the next.
    let mut prev_top: Option<String> = None;
    for snap in session.iter() {
        let top = snap
            .procs
            .iter()
            .max_by(|a, b| {
                a.cpu_pct
                    .partial_cmp(&b.cpu_pct)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|p| p.name.clone());
        if let (Some(prev), Some(curr)) = (&prev_top, &top) {
            if prev != curr {
                let age = now.duration_since(snap.t).map(|d| d.as_secs()).unwrap_or(0);
                out.push(Event {
                    kind: EventKind::TopProcChange,
                    age_secs: age,
                    detail: format!("top CPU: {} → {}", prev, curr),
                });
            }
        }
        prev_top = top;
    }

    // Currently active insights — surface as ongoing events at age 0.
    for ins in &app.insights {
        if ins.severity == Severity::Info {
            continue;
        }
        out.push(Event {
            kind: EventKind::InsightStart,
            age_secs: 0,
            detail: format!("[{}] {}", ins.severity.label(), ins.title),
        });
    }

    // Newest (smallest age) first.
    out.sort_by_key(|e| e.age_secs);
    out
}

fn window_normalized(raw: &[f32], take: usize, max: f32) -> Vec<f32> {
    let max = if max > 0.0 { max } else { 1.0 };
    let slice: &[f32] = if raw.len() > take {
        &raw[raw.len() - take..]
    } else {
        raw
    };
    slice.iter().map(|v| (v / max).clamp(0.0, 1.0)).collect()
}

fn header_style() -> Style {
    Style::default()
        .fg(p::text_muted())
        .add_modifier(Modifier::BOLD)
}
