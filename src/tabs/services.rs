use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{App, ServiceSort, Snapshot};
use crate::collect::{ServiceStatus, ServiceTick};
use crate::ui::{palette as p, widgets::panel};

pub fn draw(f: &mut Frame, area: Rect, app: &App, snap: &Snapshot) {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // sort strip
            Constraint::Min(0),    // table
            Constraint::Length(7), // detail
        ])
        .split(area);

    draw_sort_strip(f, v[0], app, snap);
    let sorted = filtered_sorted(
        &snap.services,
        app.service_sort,
        app.filter_active.as_deref(),
    );
    draw_table(f, v[1], app, &sorted);
    draw_detail(f, v[2], &sorted, app.service_sel);
}

fn draw_sort_strip(f: &mut Frame, area: Rect, app: &App, snap: &Snapshot) {
    // While typing, the strip becomes the filter input box — same
    // treatment as the Procs tab so the interaction reads identically
    // wherever the user triggers it.
    if app.filter_input {
        f.render_widget(
            Paragraph::new(crate::ui::widgets::filter_input_line(&app.filter_buf))
                .style(Style::default().bg(p::bg())),
            area,
        );
        return;
    }

    let (running, idle, failed, unknown) = counts(&snap.services);
    let mut spans: Vec<Span> = Vec::new();
    spans.push(Span::styled(" sort ", Style::default().fg(p::text_muted())));
    for s in ServiceSort::ALL.iter() {
        let active = *s == app.service_sort;
        let label = format!(" {} ", s.label());
        if active {
            spans.push(Span::styled(
                label,
                Style::default()
                    .fg(p::brand())
                    .bg(p::selection_bg())
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled("\u{25BC} ", Style::default().fg(p::brand())));
        } else {
            spans.push(Span::styled(label, Style::default().fg(p::text_primary())));
            spans.push(Span::raw(" "));
        }
    }
    spans.push(Span::raw("    "));
    // With a filter applied, lead with the match count so the narrowing
    // is visible — the status tallies below still describe the whole set.
    if let Some(needle) = app.filter_active.as_deref() {
        let visible = filtered_sorted(&snap.services, app.service_sort, Some(needle)).len();
        spans.push(Span::styled(
            format!(
                "{}/{} services  filter: \"{}\"   / f:edit  ",
                visible,
                snap.services.len(),
                needle
            ),
            Style::default().fg(p::brand()),
        ));
    } else {
        spans.push(Span::styled(
            format!("{} total  ", snap.services.len()),
            Style::default().fg(p::text_muted()),
        ));
    }
    spans.push(Span::styled(
        format!("{} running  ", running),
        Style::default()
            .fg(p::status_good())
            .add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(
        format!("{} idle  ", idle),
        Style::default().fg(p::text_muted()),
    ));
    spans.push(Span::styled(
        format!("{} failed  ", failed),
        Style::default()
            .fg(if failed > 0 {
                p::status_error()
            } else {
                p::text_muted()
            })
            .add_modifier(Modifier::BOLD),
    ));
    if unknown > 0 {
        spans.push(Span::styled(
            format!("{} unknown", unknown),
            Style::default().fg(p::text_muted()),
        ));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(p::bg())),
        area,
    );
}

fn draw_table(f: &mut Frame, area: Rect, app: &App, services: &[ServiceTick]) {
    let block = panel("SERVICES");
    let inner = block.inner(area);
    f.render_widget(block, area);

    if services.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                "No services reported (collector not yet sampled or platform unsupported).",
                Style::default().fg(p::text_muted()),
            )]))
            .style(Style::default().bg(p::bg())),
            inner,
        );
        return;
    }

    let header = Line::from(vec![
        Span::styled(format!("{:<8} ", "STATUS"), header_style()),
        Span::styled(format!("{:>7} ", "PID"), header_style()),
        Span::styled(format!("{:>5} ", "EXIT"), header_style()),
        Span::styled("NAME", header_style()),
    ]);

    let take = inner.height.saturating_sub(1) as usize;
    let sel_clamped = app.service_sel.min(services.len().saturating_sub(1));
    let start = sel_clamped.saturating_sub(take.saturating_sub(1));
    let end = (start + take).min(services.len());

    let mut lines = vec![header];
    let rendered_rows = services[start..end].iter().count();
    for (i, svc) in services[start..end].iter().enumerate() {
        let abs = start + i;
        let selected = abs == sel_clamped;
        let row_bg = if selected { p::selection_bg() } else { p::bg() };
        let row_alpha = if app.user_config.graph_fade && !selected {
            crate::ui::graph::row_fade_alpha(i, rendered_rows)
        } else {
            1.0
        };
        let (status_color, status_label) = status_style(svc.status);
        let pid_text = svc.pid.map(|p| p.to_string()).unwrap_or_else(|| "—".into());
        let exit_text = svc
            .exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "—".into());
        let exit_color = match svc.exit_code {
            Some(c) if c < 0 => p::status_warn(), // killed by signal — common on macOS
            Some(c) if c > 0 => p::status_error(),
            _ => p::text_muted(),
        };
        let spans = vec![
            Span::styled(
                format!(" {:<7} ", status_label),
                Style::default()
                    .fg(status_color)
                    .bg(row_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{:>7} ", pid_text),
                Style::default().fg(p::text_primary()).bg(row_bg),
            ),
            Span::styled(
                format!("{:>5} ", exit_text),
                Style::default().fg(exit_color).bg(row_bg),
            ),
            Span::styled(
                svc.name.clone(),
                Style::default().fg(p::text_primary()).bg(row_bg),
            ),
            Span::styled(
                fill_remainder(inner.width as usize, &svc.name),
                Style::default().bg(row_bg),
            ),
        ];
        let spans = if (row_alpha - 1.0).abs() < f32::EPSILON {
            spans
        } else {
            crate::ui::graph::fade_spans_fg(spans, p::bg(), row_alpha)
        };
        lines.push(Line::from(spans));
    }
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(p::bg())),
        inner,
    );
}

fn draw_detail(f: &mut Frame, area: Rect, services: &[ServiceTick], sel: usize) {
    let Some(svc) = services.get(sel.min(services.len().saturating_sub(1))) else {
        let block = panel("DETAIL");
        f.render_widget(block, area);
        return;
    };
    let block = panel(format!("{}  -  detail", svc.name));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let (status_color, status_label) = status_style(svc.status);
    let lines = vec![
        Line::from(vec![
            Span::styled(
                format!("{:<10} ", "status"),
                Style::default().fg(p::text_muted()),
            ),
            Span::styled(
                status_label,
                Style::default()
                    .fg(status_color)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        kv(
            "pid",
            svc.pid.map(|p| p.to_string()).unwrap_or_else(|| "—".into()),
            p::text_primary(),
        ),
        kv(
            "exit code",
            svc.exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "—".into()),
            p::text_primary(),
        ),
        kv("notes", svc.detail.clone(), p::text_muted()),
    ];
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(p::bg())),
        inner,
    );
}

fn status_style(s: ServiceStatus) -> (ratatui::style::Color, &'static str) {
    match s {
        ServiceStatus::Running => (p::status_good(), "RUN"),
        ServiceStatus::Idle => (p::text_muted(), "IDLE"),
        ServiceStatus::Failed => (p::status_error(), "FAIL"),
        ServiceStatus::Unknown => (p::border(), "?"),
    }
}

fn counts(services: &[ServiceTick]) -> (usize, usize, usize, usize) {
    let mut r = 0;
    let mut i = 0;
    let mut f = 0;
    let mut u = 0;
    for s in services {
        match s.status {
            ServiceStatus::Running => r += 1,
            ServiceStatus::Idle => i += 1,
            ServiceStatus::Failed => f += 1,
            ServiceStatus::Unknown => u += 1,
        }
    }
    (r, i, f, u)
}

/// Filter then sort the service list. `filter` is a case-insensitive
/// substring match against name / detail — detail carries systemd's
/// DESCRIPTION, which is often what a user remembers a unit by. Public
/// so the App key handler can clamp `service_sel` to the same view
/// (issue #20).
pub(crate) fn filtered_sorted(
    services: &[ServiceTick],
    key: ServiceSort,
    filter: Option<&str>,
) -> Vec<ServiceTick> {
    let needle = filter.map(|s| s.to_lowercase());
    let matched: Vec<ServiceTick> = services
        .iter()
        .filter(|s| match needle.as_deref() {
            None => true,
            Some(n) => s.name.to_lowercase().contains(n) || s.detail.to_lowercase().contains(n),
        })
        .cloned()
        .collect();
    sort_services(&matched, key)
}

fn sort_services(services: &[ServiceTick], key: ServiceSort) -> Vec<ServiceTick> {
    let mut out = services.to_vec();
    match key {
        ServiceSort::Name => out.sort_by(|a, b| a.name.cmp(&b.name)),
        ServiceSort::Status => out.sort_by(|a, b| {
            // Failed first, then Running, then Idle, then Unknown.
            let rank = |s: &ServiceTick| match s.status {
                ServiceStatus::Failed => 0,
                ServiceStatus::Running => 1,
                ServiceStatus::Idle => 2,
                ServiceStatus::Unknown => 3,
            };
            rank(a).cmp(&rank(b)).then_with(|| a.name.cmp(&b.name))
        }),
        ServiceSort::Pid => out.sort_by(|a, b| match (a.pid, b.pid) {
            (Some(pa), Some(pb)) => pa.cmp(&pb),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.name.cmp(&b.name),
        }),
    }
    out
}

fn kv(k: &str, v: String, val_color: ratatui::style::Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{:<10} ", k), Style::default().fg(p::text_muted())),
        Span::styled(v, Style::default().fg(val_color)),
    ])
}

fn fill_remainder(width: usize, used: &str) -> String {
    // 1 + 7 + 1 + 7 + 1 + 5 + 1 = 23 chars before the name column
    let used_w = 23 + used.chars().count();
    if width > used_w {
        " ".repeat(width - used_w)
    } else {
        String::new()
    }
}

fn header_style() -> Style {
    Style::default()
        .fg(p::text_muted())
        .add_modifier(Modifier::BOLD)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(name: &str, status: ServiceStatus, pid: Option<u32>) -> ServiceTick {
        ServiceTick {
            name: name.into(),
            status,
            pid,
            exit_code: None,
            detail: String::new(),
        }
    }

    fn names(v: &[ServiceTick]) -> Vec<&str> {
        v.iter().map(|s| s.name.as_str()).collect()
    }

    fn fixture() -> Vec<ServiceTick> {
        vec![
            s("zeta.service", ServiceStatus::Idle, None),
            s("alpha.service", ServiceStatus::Failed, None),
            s("beta.service", ServiceStatus::Running, Some(42)),
            s("gamma.service", ServiceStatus::Running, Some(7)),
            s("delta.service", ServiceStatus::Unknown, None),
        ]
    }

    #[test]
    fn sort_by_name_ascending() {
        let out = sort_services(&fixture(), ServiceSort::Name);
        assert_eq!(
            names(&out),
            vec![
                "alpha.service",
                "beta.service",
                "delta.service",
                "gamma.service",
                "zeta.service",
            ]
        );
    }

    #[test]
    fn sort_by_status_failed_first_then_running() {
        let out = sort_services(&fixture(), ServiceSort::Status);
        // Order: Failed, Running (name-tiebreak), Idle, Unknown.
        assert_eq!(
            names(&out),
            vec![
                "alpha.service", // Failed
                "beta.service",  // Running
                "gamma.service", // Running
                "zeta.service",  // Idle
                "delta.service", // Unknown
            ]
        );
    }

    #[test]
    fn sort_by_pid_ascending_with_unset_last() {
        let out = sort_services(&fixture(), ServiceSort::Pid);
        // Some(7), Some(42), then None entries fall back to name order.
        assert_eq!(
            names(&out),
            vec![
                "gamma.service", // pid 7
                "beta.service",  // pid 42
                "alpha.service", // None — alphabetic
                "delta.service",
                "zeta.service",
            ]
        );
    }

    #[test]
    fn sort_empty_is_empty() {
        assert!(sort_services(&[], ServiceSort::Name).is_empty());
        assert!(sort_services(&[], ServiceSort::Status).is_empty());
    }

    #[test]
    fn counts_partition_correctly() {
        let (r, i, f, u) = counts(&fixture());
        assert_eq!(r, 2);
        assert_eq!(i, 1);
        assert_eq!(f, 1);
        assert_eq!(u, 1);
    }

    // ── filter (issue #20) ──────────────────────────────────────────────

    #[test]
    fn filter_none_returns_the_full_sorted_list() {
        let out = filtered_sorted(&fixture(), ServiceSort::Name, None);
        assert_eq!(out.len(), 5);
        assert_eq!(names(&out)[0], "alpha.service");
    }

    #[test]
    fn filter_matches_name_case_insensitively() {
        let out = filtered_sorted(&fixture(), ServiceSort::Name, Some("ETA"));
        assert_eq!(names(&out), vec!["beta.service", "zeta.service"]);
    }

    #[test]
    fn filter_matches_detail_text() {
        // systemd's DESCRIPTION lands in `detail`, and it's often what a
        // user remembers a unit by rather than its unit name.
        let mut fx = fixture();
        fx[0].detail = "OpenSSH server daemon".into();
        let out = filtered_sorted(&fx, ServiceSort::Name, Some("openssh"));
        assert_eq!(names(&out), vec!["zeta.service"]);
    }

    #[test]
    fn filter_applies_before_sorting() {
        // Sort must order the matches, not the whole list — otherwise the
        // selection index and the visible rows disagree.
        let out = filtered_sorted(&fixture(), ServiceSort::Status, Some("a."));
        assert_eq!(
            names(&out),
            vec![
                "alpha.service", // Failed
                "beta.service",  // Running
                "gamma.service", // Running
                "zeta.service",  // Idle
                "delta.service", // Unknown
            ]
        );
    }

    #[test]
    fn filter_with_no_matches_is_empty() {
        assert!(filtered_sorted(&fixture(), ServiceSort::Name, Some("nope")).is_empty());
    }
}
