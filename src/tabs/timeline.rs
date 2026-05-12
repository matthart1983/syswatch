use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{App, Snapshot};
use crate::insights::{Insight, Severity};
use crate::ui::{
    palette as p,
    widgets::{panel, sparkline},
};

pub fn draw(f: &mut Frame, area: Rect, app: &App, _snap: &Snapshot) {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(12), // activity strips (5 × 2 rows)
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
    let gpu = window_normalized(&app.history.gpu_util.to_vec(), take, 100.0);

    let strips = [
        ("cpu  ", &cpu, p::status_good()),
        ("mem  ", &mem, p::status_warn()),
        ("io   ", &io, p::brand()),
        ("net  ", &net, p::tx_rate()),
        ("gpu  ", &gpu, p::status_error()),
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

    let session = app.history.session.to_vec();
    let events = derive_events(&session, &app.insights);
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

fn derive_events(session: &[Snapshot], insights: &[Insight]) -> Vec<Event> {
    // Walk the session, comparing adjacent snapshots for top-proc changes.
    // Insight transitions need recomputing the insight set per tick — too
    // expensive for now, so we surface only the *current* active insights and
    // top-proc churn from the session.
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
    for ins in insights {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_normalized_empty_input_yields_empty() {
        assert!(window_normalized(&[], 10, 100.0).is_empty());
    }

    #[test]
    fn window_normalized_zero_max_avoids_division_by_zero() {
        // When the peak is 0 (e.g. a freshly-started session with no
        // activity yet), max gets clamped to 1.0 so we don't NaN out.
        let out = window_normalized(&[0.0, 0.0, 0.0], 10, 0.0);
        assert_eq!(out, vec![0.0, 0.0, 0.0]);
    }

    #[test]
    fn window_normalized_negative_max_treated_as_safe() {
        // Negative max isn't a real case but the guard `max > 0.0` rejects
        // it the same way as zero, so the output should still be sane.
        let out = window_normalized(&[0.5], 10, -5.0);
        assert_eq!(out, vec![0.5]);
    }

    #[test]
    fn window_normalized_takes_tail_when_raw_exceeds_take() {
        let raw = [10.0, 20.0, 25.0, 50.0, 75.0];
        let out = window_normalized(&raw, 3, 100.0);
        assert_eq!(out, vec![0.25, 0.5, 0.75]);
    }

    #[test]
    fn window_normalized_returns_full_input_when_shorter_than_take() {
        let raw = [25.0, 50.0];
        let out = window_normalized(&raw, 10, 100.0);
        assert_eq!(out, vec![0.25, 0.5]);
    }

    #[test]
    fn window_normalized_clamps_values_above_max_to_one() {
        let out = window_normalized(&[150.0, 50.0], 10, 100.0);
        assert_eq!(out, vec![1.0, 0.5]);
    }

    #[test]
    fn window_normalized_clamps_negative_values_to_zero() {
        let out = window_normalized(&[-10.0, 50.0], 10, 100.0);
        assert_eq!(out, vec![0.0, 0.5]);
    }

    #[test]
    fn mark_cursor_no_op_when_scrub_past_series_end() {
        // scrub_offset beyond series length must not panic and must not
        // corrupt spans — important since scrub state and series len are
        // independent and can briefly disagree across resamples.
        let label = Span::styled("cpu  ", Style::default());
        let spark = Span::styled("▁▂▃▄▅".to_string(), Style::default());
        let mut spans = vec![label.clone(), spark.clone()];
        mark_cursor(&mut spans, 5, 100, p::brand());
        assert_eq!(spans[1].content, spark.content);
    }

    #[test]
    fn mark_cursor_no_op_on_empty_series() {
        let mut spans = vec![Span::styled("cpu  ", Style::default())];
        mark_cursor(&mut spans, 0, 0, p::brand());
        // No second span to touch; just confirm we don't panic.
        assert_eq!(spans.len(), 1);
    }

    #[test]
    fn mark_cursor_replaces_cell_with_vertical_bar() {
        // 5-char sparkline, scrub_offset 2 → cursor_idx = 5 - 1 - 2 = 2.
        let label = Span::styled("cpu  ", Style::default());
        let spark = Span::styled("abcde".to_string(), Style::default());
        let mut spans = vec![label, spark];
        mark_cursor(&mut spans, 5, 2, p::brand());
        let chars: Vec<char> = spans[1].content.chars().collect();
        assert_eq!(chars[2], '\u{2503}');
        // Other positions untouched.
        assert_eq!(chars[0], 'a');
        assert_eq!(chars[1], 'b');
        assert_eq!(chars[3], 'd');
        assert_eq!(chars[4], 'e');
    }

    // ── derive_events ──────────────────────────────────────
    //
    // Invariant-style tests covering the cases proptest would generate.
    // Adding the `proptest` dev-dep would give true generative coverage,
    // but the function's input space is small enough that hand-rolled
    // cases hit the same invariants without the dependency.

    use crate::app::TabId;
    use crate::collect::ProcTick;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn ts(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn snap_with_top(t: SystemTime, name: &str) -> Snapshot {
        Snapshot {
            t,
            procs: vec![ProcTick {
                name: name.to_string(),
                cpu_pct: 50.0,
                ..ProcTick::default()
            }],
            ..Snapshot::default()
        }
    }

    fn insight(sev: Severity, title: &str) -> Insight {
        Insight {
            severity: sev,
            title: title.to_string(),
            body: Vec::new(),
            suggested_tab: TabId::Overview,
        }
    }

    #[test]
    fn derive_events_empty_session_returns_empty() {
        // Even with insights present, an empty session blocks event
        // generation — we need at least 2 ticks to derive top-proc churn,
        // and surfacing insights without that context is too noisy.
        let insights = vec![insight(Severity::Warn, "alone")];
        assert!(derive_events(&[], &insights).is_empty());
    }

    #[test]
    fn derive_events_single_snapshot_returns_empty() {
        let session = vec![snap_with_top(ts(100), "foo")];
        assert!(derive_events(&session, &[]).is_empty());
    }

    #[test]
    fn derive_events_stable_top_proc_emits_no_proc_events() {
        let session = vec![
            snap_with_top(ts(100), "foo"),
            snap_with_top(ts(101), "foo"),
            snap_with_top(ts(102), "foo"),
        ];
        assert!(derive_events(&session, &[]).is_empty());
    }

    #[test]
    fn derive_events_one_event_per_top_proc_change() {
        // 3 transitions across 4 ticks → 3 events.
        let session = vec![
            snap_with_top(ts(100), "a"),
            snap_with_top(ts(101), "b"),
            snap_with_top(ts(102), "c"),
            snap_with_top(ts(103), "d"),
        ];
        let events = derive_events(&session, &[]);
        assert_eq!(events.len(), 3);
        // All carry the "A → B" arrow detail.
        for e in &events {
            assert!(e.detail.contains("→"), "missing arrow: {}", e.detail);
        }
    }

    #[test]
    fn derive_events_filters_info_severity_insights() {
        let session = vec![snap_with_top(ts(100), "foo"), snap_with_top(ts(101), "foo")];
        let insights = vec![insight(Severity::Info, "no anomalies")];
        assert!(derive_events(&session, &insights).is_empty());
    }

    #[test]
    fn derive_events_emits_warn_and_crit_insights() {
        let session = vec![snap_with_top(ts(100), "foo"), snap_with_top(ts(101), "foo")];
        let insights = vec![
            insight(Severity::Warn, "swap thrash"),
            insight(Severity::Crit, "vram pinned"),
        ];
        let events = derive_events(&session, &insights);
        assert_eq!(events.len(), 2);
        let details: Vec<&str> = events.iter().map(|e| e.detail.as_str()).collect();
        assert!(details
            .iter()
            .any(|d| d.contains("[WARN]") && d.contains("swap thrash")));
        assert!(details
            .iter()
            .any(|d| d.contains("[CRIT]") && d.contains("vram pinned")));
    }

    #[test]
    fn derive_events_sorted_by_age_ascending() {
        // Newest-first means smaller age_secs first. The function sorts
        // before returning — verify the invariant holds for a session
        // with multiple churn events spread across distinct timestamps.
        let session = vec![
            snap_with_top(ts(100), "a"),
            snap_with_top(ts(110), "b"),
            snap_with_top(ts(120), "c"),
            snap_with_top(ts(130), "d"),
        ];
        let events = derive_events(&session, &[]);
        for pair in events.windows(2) {
            assert!(
                pair[0].age_secs <= pair[1].age_secs,
                "ordering broken: {} then {}",
                pair[0].age_secs,
                pair[1].age_secs
            );
        }
    }

    #[test]
    fn derive_events_does_not_panic_on_timestamps_running_backwards() {
        // Real clocks can jump backwards (NTP corrections, manual resets).
        // duration_since returns Err in that case; the function's `.unwrap_or(0)`
        // must keep it from panicking.
        let session = vec![snap_with_top(ts(200), "foo"), snap_with_top(ts(100), "bar")];
        let events = derive_events(&session, &[]);
        // Just confirm we got back a valid Vec; specific values aren't
        // load-bearing once duration_since errored.
        assert!(!events.is_empty());
    }

    #[test]
    fn derive_events_handles_snapshots_with_no_procs() {
        // A snapshot with no procs has no top — the prev_top stays None
        // and we emit nothing. Must not panic on the iterator chain.
        let session = vec![Snapshot::default(), Snapshot::default()];
        assert!(derive_events(&session, &[]).is_empty());
    }

    #[test]
    fn derive_events_change_detected_only_on_distinct_neighbors() {
        // a → b → a → b should yield 3 changes (one per transition).
        let session = vec![
            snap_with_top(ts(100), "a"),
            snap_with_top(ts(101), "b"),
            snap_with_top(ts(102), "a"),
            snap_with_top(ts(103), "b"),
        ];
        let events = derive_events(&session, &[]);
        assert_eq!(events.len(), 3);
    }
}
