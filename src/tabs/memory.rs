use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{App, Snapshot};
use crate::collect::ProcTick;
use crate::ui::{
    graph::GraphStyle,
    palette as p,
    widgets::{block_bar_styled, human_bytes, mem_pct, panel},
};

pub fn draw(f: &mut Frame, area: Rect, app: &App, snap: &Snapshot) {
    // Drop the SWAP panel entirely when no swap is configured — those 7 rows
    // go to the process list instead (issue #12).
    let has_swap = snap.mem.swap_total_bytes > 0;
    // The filter strip costs a process row, so it only appears once there
    // is a filter to show or edit — same reasoning as issue #12. The key
    // itself is advertised in the footer and help popup (issue #20).
    let show_filter = app.filter_input || app.filter_active.is_some();

    let mut constraints = vec![Constraint::Length(7)];
    if has_swap {
        constraints.push(Constraint::Length(7));
    }
    if show_filter {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Min(0));

    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let mut row = 0;
    draw_ram_bar(f, v[row], snap, app.graph_style);
    row += 1;
    if has_swap {
        draw_swap(f, v[row], snap, app.graph_style);
        row += 1;
    }
    if show_filter {
        draw_filter_strip(f, v[row], app, snap);
        row += 1;
    }
    draw_proc_breakdown(f, v[row], app, snap);
}

/// Filter prompt while typing; match count once applied.
fn draw_filter_strip(f: &mut Frame, area: Rect, app: &App, snap: &Snapshot) {
    let line = if app.filter_input {
        crate::ui::widgets::filter_input_line(&app.filter_buf)
    } else {
        let needle = app.filter_active.as_deref().unwrap_or_default();
        Line::from(Span::styled(
            format!(
                " {}/{} procs  filter: \"{}\"   / f:edit",
                filtered_procs(&snap.procs, Some(needle)).len(),
                snap.procs.len(),
                needle
            ),
            Style::default().fg(p::brand()),
        ))
    };
    f.render_widget(
        Paragraph::new(line).style(Style::default().bg(p::bg())),
        area,
    );
}

/// Narrow the process list by the shared filter, preserving input order —
/// the breakdown does its own memory-weighted sort afterwards, so unlike
/// the Procs tab this must not impose one.
fn filtered_procs(procs: &[ProcTick], filter: Option<&str>) -> Vec<ProcTick> {
    let needle = filter.map(|s| s.to_lowercase());
    procs
        .iter()
        .filter(|p| match needle.as_deref() {
            None => true,
            Some(n) => crate::tabs::procs::proc_matches(p, n),
        })
        .cloned()
        .collect()
}

fn draw_ram_bar(f: &mut Frame, area: Rect, snap: &Snapshot, style: GraphStyle) {
    let block = panel("RAM");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let total = snap.mem.total_bytes.max(1);
    let used = snap.mem.used_bytes;
    let avail = snap.mem.available_bytes;
    let pct = used as f32 / total as f32;
    let color = if pct >= 0.9 {
        p::status_error()
    } else if pct >= 0.7 {
        p::status_warn()
    } else {
        p::status_good()
    };

    let mut header_spans = vec![
        Span::styled("used ", Style::default().fg(p::text_muted())),
        Span::styled(
            human_bytes(used),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" / ", Style::default().fg(p::text_muted())),
        Span::styled(human_bytes(total), Style::default().fg(p::text_primary())),
        Span::styled(
            format!("   ({:>4.1}%)", pct * 100.0),
            Style::default().fg(p::text_muted()),
        ),
        Span::styled("    available ", Style::default().fg(p::text_muted())),
        Span::styled(human_bytes(avail), Style::default().fg(p::text_primary())),
    ];
    // PSI: stall time is the honest pressure signal — %used can be
    // high while nothing is starved, and vice versa.
    if let Some(psi) = &snap.pressure {
        let stall_color = if psi.mem_full >= 5.0 {
            p::status_error()
        } else if psi.mem_some >= 10.0 {
            p::status_warn()
        } else {
            p::text_primary()
        };
        header_spans.push(Span::styled(
            "    stall ",
            Style::default().fg(p::text_muted()),
        ));
        header_spans.push(Span::styled(
            format!("{:.1}% some / {:.1}% full", psi.mem_some, psi.mem_full),
            Style::default().fg(stall_color),
        ));
    }
    let header = Line::from(header_spans);
    let bar = block_bar_styled(pct, inner.width, color, style);
    f.render_widget(
        Paragraph::new(vec![header, Line::from(""), bar]).style(Style::default().bg(p::bg())),
        inner,
    );
}

fn draw_swap(f: &mut Frame, area: Rect, snap: &Snapshot, style: GraphStyle) {
    let block = panel("Swap");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let total = snap.mem.swap_total_bytes;
    let used = snap.mem.swap_used_bytes;
    let pct = if total > 0 {
        used as f32 / total as f32
    } else {
        0.0
    };
    let color = if pct >= 0.75 {
        p::status_error()
    } else if pct >= 0.25 {
        p::status_warn()
    } else {
        p::status_good()
    };

    let header = if total == 0 {
        Line::from(vec![Span::styled(
            "no swap configured",
            Style::default().fg(p::text_muted()),
        )])
    } else {
        Line::from(vec![
            Span::styled("used ", Style::default().fg(p::text_muted())),
            Span::styled(
                human_bytes(used),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" / ", Style::default().fg(p::text_muted())),
            Span::styled(human_bytes(total), Style::default().fg(p::text_primary())),
            Span::styled(
                format!("   ({:>4.1}%)", pct * 100.0),
                Style::default().fg(p::text_muted()),
            ),
        ])
    };
    let bar = block_bar_styled(pct, inner.width, color, style);
    f.render_widget(
        Paragraph::new(vec![header, Line::from(""), bar]).style(Style::default().bg(p::bg())),
        inner,
    );
}

/// Per-process memory breakdown. RSS double-counts shared pages, so
/// where the platform sampler delivered honest accounting we lead with
/// it: phys_footprint on macOS (Activity Monitor's Memory column),
/// PSS + private/shared/swap from smaps_rollup on Linux. Detail is
/// sampled for the top procs by RSS and only for procs the user can
/// inspect without sudo — rows without it show "—" and fall back to
/// RSS-based ordering.
fn draw_proc_breakdown(f: &mut Frame, area: Rect, app: &App, snap: &Snapshot) {
    let total_mem = snap.mem.total_bytes.max(1);

    // Column sets keyed off what the sampler could actually read this
    // session — same pattern as the procs tab's NET / GPU columns.
    let show_footprint = snap.procs.iter().any(|p| p.mem_footprint.is_some());
    let show_smaps = snap.procs.iter().any(|p| p.mem_pss.is_some());

    let block = panel(if show_footprint {
        "Process memory breakdown (footprint = real pressure; rss double-counts shared)"
    } else if show_smaps {
        "Process memory breakdown (pss sums to real use; private frees on exit)"
    } else {
        "Process memory breakdown (by RSS)"
    });
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Order by the most honest metric available per row, falling back
    // to RSS so undetailed rows still rank sensibly.
    let mut sorted = filtered_procs(&snap.procs, app.filter_active.as_deref());
    sorted.sort_by_key(|p| std::cmp::Reverse(p.mem_footprint.or(p.mem_pss).unwrap_or(p.mem_rss)));
    let take = inner.height.saturating_sub(1) as usize;

    let header_style = Style::default()
        .fg(p::text_muted())
        .add_modifier(Modifier::BOLD);
    let mut header: Vec<Span> = vec![
        Span::styled(format!("{:>7} ", "PID"), header_style),
        Span::styled(format!("{:<10} ", "USER"), header_style),
        // Width 5 to match the `{:>5.1}` data cell — see issue #11.
        Span::styled(format!("{:>5} ", "%MEM"), header_style),
    ];
    if show_footprint {
        header.push(Span::styled(format!("{:>10} ", "FOOTPRNT"), header_style));
    }
    if show_smaps {
        header.push(Span::styled(format!("{:>10} ", "PSS"), header_style));
        header.push(Span::styled(format!("{:>10} ", "PRIVATE"), header_style));
        header.push(Span::styled(format!("{:>10} ", "SHARED"), header_style));
        header.push(Span::styled(format!("{:>10} ", "SWAP"), header_style));
    }
    let show_peak = snap.procs.iter().any(|p| p.mem_peak.is_some());
    if show_peak {
        header.push(Span::styled(format!("{:>10} ", "PEAK"), header_style));
    }
    header.push(Span::styled(format!("{:>10} ", "RSS"), header_style));
    if !show_footprint && !show_smaps {
        header.push(Span::styled(format!("{:>10} ", "VIRT"), header_style));
    }
    header.push(Span::styled("COMMAND", header_style));
    let mut lines: Vec<Line> = vec![Line::from(header)];

    // None → "—": the proc exists but detail wasn't readable (other
    // user without sudo) or it ranked below the sampler's top-N cap.
    let detail = |v: Option<u64>| match v {
        Some(b) => human_bytes(b),
        None => "—".into(),
    };
    let detail_fg = |v: Option<u64>| {
        if v.is_some() {
            p::brand()
        } else {
            p::text_muted()
        }
    };

    let rendered_rows = sorted.iter().take(take).count();
    for (i, proc_) in sorted.iter().take(take).enumerate() {
        let row_alpha = if app.user_config.graph_fade {
            crate::ui::graph::row_fade_alpha(i, rendered_rows)
        } else {
            1.0
        };
        let mut spans = vec![
            Span::styled(
                format!("{:>7} ", proc_.pid),
                Style::default().fg(p::text_primary()),
            ),
            Span::styled(
                format!("{:<10.10} ", proc_.user),
                Style::default().fg(p::text_muted()),
            ),
            Span::styled(
                format!("{:>5.1} ", mem_pct(proc_.mem_rss, total_mem)),
                Style::default().fg(p::text_primary()),
            ),
        ];
        if show_footprint {
            spans.push(Span::styled(
                format!("{:>10} ", detail(proc_.mem_footprint)),
                Style::default().fg(detail_fg(proc_.mem_footprint)),
            ));
        }
        if show_smaps {
            for v in [
                proc_.mem_pss,
                proc_.mem_private,
                proc_.mem_shared,
                proc_.mem_swap,
            ] {
                spans.push(Span::styled(
                    format!("{:>10} ", detail(v)),
                    Style::default().fg(detail_fg(v)),
                ));
            }
        }
        if show_peak {
            spans.push(Span::styled(
                format!("{:>10} ", detail(proc_.mem_peak)),
                Style::default().fg(detail_fg(proc_.mem_peak)),
            ));
        }
        spans.push(Span::styled(
            format!("{:>10} ", human_bytes(proc_.mem_rss)),
            Style::default().fg(if show_footprint || show_smaps {
                p::text_muted()
            } else {
                p::brand()
            }),
        ));
        if !show_footprint && !show_smaps {
            spans.push(Span::styled(
                format!("{:>10} ", human_bytes(proc_.mem_virt)),
                Style::default().fg(p::text_muted()),
            ));
        }
        spans.push(Span::styled(
            proc_.name.clone(),
            Style::default().fg(p::text_primary()),
        ));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, name: &str, cmd: &str, user: &str, rss: u64) -> ProcTick {
        ProcTick {
            pid,
            name: name.into(),
            cmd: cmd.into(),
            user: user.into(),
            mem_rss: rss,
            ..Default::default()
        }
    }

    fn fixture() -> Vec<ProcTick> {
        vec![
            proc(1, "kernel_task", "", "root", 900),
            proc(
                2,
                "Chrome Helper",
                "/Applications/Chrome --renderer",
                "matt",
                500,
            ),
            proc(3, "postgres", "postgres: writer", "postgres", 300),
        ]
    }

    fn names(v: &[ProcTick]) -> Vec<&str> {
        v.iter().map(|p| p.name.as_str()).collect()
    }

    // ── filter (issue #20) ──────────────────────────────────────────────

    #[test]
    fn no_filter_keeps_every_proc() {
        assert_eq!(filtered_procs(&fixture(), None).len(), 3);
    }

    #[test]
    fn filter_matches_name_cmd_or_user_case_insensitively() {
        assert_eq!(
            names(&filtered_procs(&fixture(), Some("CHROME"))),
            ["Chrome Helper"]
        );
        assert_eq!(
            names(&filtered_procs(&fixture(), Some("renderer"))),
            ["Chrome Helper"]
        );
        assert_eq!(
            names(&filtered_procs(&fixture(), Some("root"))),
            ["kernel_task"]
        );
    }

    /// The breakdown re-sorts by footprint/PSS/RSS afterwards, so the
    /// filter must leave input order alone — imposing the Procs tab's
    /// sort here would silently reorder the memory ranking.
    #[test]
    fn filter_preserves_input_order() {
        let out = filtered_procs(&fixture(), Some("e"));
        assert_eq!(names(&out), ["kernel_task", "Chrome Helper", "postgres"]);
    }

    #[test]
    fn filter_with_no_matches_is_empty() {
        assert!(filtered_procs(&fixture(), Some("zzz")).is_empty());
    }
}
