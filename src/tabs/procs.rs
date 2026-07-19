use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{App, ProcSort, Snapshot};
use crate::collect::ProcTick;
use crate::ui::{
    palette as p,
    widgets::{human_bytes, human_rate, mem_pct, panel},
};

pub fn draw(f: &mut Frame, area: Rect, app: &App, snap: &Snapshot) {
    // The drill-in pane is toggleable (`d`); hidden, its 9 rows go to the
    // process list (issue #13).
    let mut constraints = vec![
        Constraint::Length(1), // sort strip / filter input
        Constraint::Min(0),    // process table
    ];
    if app.proc_show_detail {
        constraints.push(Constraint::Length(9)); // drill-in
    }
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    draw_sort_strip(f, v[0], app, snap);
    let view = filtered_sorted(
        &snap.procs,
        app.proc_sort,
        app.proc_filter_active.as_deref(),
    );
    let total_mem = snap.mem.total_bytes.max(1);
    draw_table(f, v[1], app, &view, total_mem, snap.net_rates_estimated);
    if app.proc_show_detail {
        draw_drill_in(f, v[2], &view, app.proc_sel, total_mem);
    }
}

/// Filter then sort the proc list. `filter` is a case-insensitive
/// substring match against name / cmd / user. Public so the App key
/// handler can use the same view to clamp `proc_sel`.
pub(crate) fn filtered_sorted(
    procs: &[ProcTick],
    key: ProcSort,
    filter: Option<&str>,
) -> Vec<ProcTick> {
    let needle = filter.map(|s| s.to_lowercase());
    let mut out: Vec<ProcTick> = procs
        .iter()
        .filter(|p| match needle.as_deref() {
            None => true,
            Some(n) => {
                p.name.to_lowercase().contains(n)
                    || p.cmd.to_lowercase().contains(n)
                    || p.user.to_lowercase().contains(n)
            }
        })
        .cloned()
        .collect();
    sort_in_place(&mut out, key);
    out
}

fn sort_in_place(out: &mut [ProcTick], key: ProcSort) {
    match key {
        ProcSort::Cpu => out.sort_by(|a, b| {
            b.cpu_pct
                .partial_cmp(&a.cpu_pct)
                .unwrap_or(std::cmp::Ordering::Equal)
        }),
        ProcSort::Rss => out.sort_by(|a, b| b.mem_rss.cmp(&a.mem_rss)),
        ProcSort::Io => out.sort_by(|a, b| {
            let total = |p: &ProcTick| p.io_read_rate + p.io_write_rate;
            total(b)
                .partial_cmp(&total(a))
                .unwrap_or(std::cmp::Ordering::Equal)
        }),
        ProcSort::Start => out.sort_by(|a, b| b.start_time.cmp(&a.start_time)),
        ProcSort::Name => out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
        ProcSort::Gpu => out.sort_by(|a, b| {
            // None → 0.0 so procs without GPU attribution sink to the
            // bottom rather than appearing as a top match.
            let av = a.gpu_pct.unwrap_or(0.0);
            let bv = b.gpu_pct.unwrap_or(0.0);
            bv.partial_cmp(&av).unwrap_or(std::cmp::Ordering::Equal)
        }),
        ProcSort::Net => out.sort_by(|a, b| {
            let total = |p: &ProcTick| p.net_rx_rate.unwrap_or(0.0) + p.net_tx_rate.unwrap_or(0.0);
            total(b)
                .partial_cmp(&total(a))
                .unwrap_or(std::cmp::Ordering::Equal)
        }),
    }
}

fn draw_sort_strip(f: &mut Frame, area: Rect, app: &App, snap: &Snapshot) {
    // While typing into the filter, the strip becomes a single-line
    // input box — no sort chips, just the prompt and a cursor.
    if app.proc_filter_input {
        let line = Line::from(vec![
            Span::styled(" / ", Style::default().fg(p::brand()).bold()),
            Span::styled(
                app.proc_filter_buf.clone(),
                Style::default().fg(p::text_primary()),
            ),
            Span::styled(
                "▏",
                Style::default().fg(p::brand()).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "    Enter:apply  Esc:cancel",
                Style::default().fg(p::text_muted()),
            ),
        ]);
        f.render_widget(
            Paragraph::new(line).style(Style::default().bg(p::bg())),
            area,
        );
        return;
    }

    let mut spans: Vec<Span> = Vec::new();
    spans.push(Span::styled(" sort ", Style::default().fg(p::text_muted())));
    for s in ProcSort::ALL.iter() {
        let active = *s == app.proc_sort;
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
    // Show match count when a filter is applied so the user can see
    // how aggressive the narrowing is at a glance.
    let count_text = if let Some(f) = app.proc_filter_active.as_deref() {
        let visible = filtered_sorted(&snap.procs, app.proc_sort, Some(f)).len();
        format!(
            "    {}/{} procs  filter: \"{}\"   / f:edit  s:sort  d:detail  ↑↓:select",
            visible,
            snap.procs.len(),
            f
        )
    } else {
        format!(
            "    {} procs   / f:filter  s:sort  d:detail  ↑↓:select",
            snap.procs.len()
        )
    };
    spans.push(Span::styled(
        count_text,
        Style::default().fg(p::text_muted()),
    ));
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(p::bg())),
        area,
    );
}

fn draw_table(
    f: &mut Frame,
    area: Rect,
    app: &App,
    procs: &[ProcTick],
    total_mem: u64,
    net_estimated: bool,
) {
    let block = panel("PROCESSES");
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Show NET / GPU columns only when at least one proc has data —
    // saves columns on platforms where the source isn't available.
    let show_net = procs.iter().any(|p| p.net_rx_rate.is_some());
    let show_gpu = procs
        .iter()
        .any(|p| p.gpu_pct.is_some() || p.gpu_mem_bytes.is_some());
    let mut header_spans: Vec<Span> = vec![
        Span::styled(format!("{:>7} ", "PID"), header_style()),
        Span::styled(format!("{:>7} ", "PPID"), header_style()),
        Span::styled(format!("{:<14} ", "USER"), header_style()),
        // Width 5 to match the `{:>5.1}` data cells below — a width-6 header
        // here drifted every column after USER one cell right (issue #11).
        Span::styled(format!("{:>5} ", "%CPU"), header_style()),
        // Inline CPU-usage history, btop-style (issue #10).
        Span::styled(format!("{:<8} ", "CPU HIST"), header_style()),
        Span::styled(format!("{:>5} ", "%MEM"), header_style()),
    ];
    // VIRT only when neither extras are shown — keeps the row from
    // sprawling past 120 cols when both NET and GPU are populated.
    if !show_net && !show_gpu {
        header_spans.push(Span::styled(format!("{:>9} ", "VIRT"), header_style()));
    }
    header_spans.push(Span::styled(format!("{:<5} ", "STATE"), header_style()));
    header_spans.push(Span::styled(format!("{:>10} ", "R/s"), header_style()));
    header_spans.push(Span::styled(format!("{:>10} ", "W/s"), header_style()));
    if show_net {
        // `~` marks the connection-count estimate; measured rates
        // (nettop on macOS) render unmarked.
        let (rx_h, tx_h) = if net_estimated {
            ("~NET ↓/s", "~NET ↑/s")
        } else {
            ("NET ↓/s", "NET ↑/s")
        };
        header_spans.push(Span::styled(format!("{:>10} ", rx_h), header_style()));
        header_spans.push(Span::styled(format!("{:>10} ", tx_h), header_style()));
    }
    if show_gpu {
        header_spans.push(Span::styled(format!("{:>5} ", "%GPU"), header_style()));
        header_spans.push(Span::styled(format!("{:>9} ", "GPU MEM"), header_style()));
    }
    header_spans.push(Span::styled("COMMAND", header_style()));
    let header = Line::from(header_spans);

    let take = inner.height.saturating_sub(1) as usize;
    let sel_clamped = app.proc_sel.min(procs.len().saturating_sub(1));
    // Scroll: keep selection visible.
    let start = sel_clamped.saturating_sub(take.saturating_sub(1));
    let end = (start + take).min(procs.len());

    let mut lines = vec![header];
    let rendered_rows = procs[start..end].iter().count();
    for (i, proc_) in procs[start..end].iter().enumerate() {
        let abs = start + i;
        let selected = abs == sel_clamped;
        let row_alpha = if app.user_config.graph_fade && !selected {
            crate::ui::graph::row_fade_alpha(i, rendered_rows)
        } else {
            1.0
        };
        let row_bg = if selected { p::selection_bg() } else { p::bg() };
        let dot_color = if proc_.cpu_pct >= 30.0 {
            p::status_warn()
        } else if matches!(proc_.state, 'R') {
            p::status_good()
        } else if matches!(proc_.state, 'Z') {
            p::status_error()
        } else {
            p::border()
        };
        let cpu_color = if proc_.cpu_pct >= 30.0 {
            p::status_warn()
        } else {
            p::text_primary()
        };
        let state_color = match proc_.state {
            'R' => p::status_good(),
            'S' | 'I' => p::text_primary(),
            'Z' => p::status_error(),
            _ => p::text_muted(),
        };
        let mut spans: Vec<Span> = vec![
            Span::styled(
                format!("{:>7} ", proc_.pid),
                Style::default().fg(p::text_primary()).bg(row_bg),
            ),
            Span::styled(
                format!("{:>7} ", proc_.ppid),
                Style::default().fg(p::text_muted()).bg(row_bg),
            ),
            Span::styled(
                format!("{:<14.14} ", proc_.user),
                Style::default().fg(p::text_muted()).bg(row_bg),
            ),
            Span::styled(
                format!("{:>5.1} ", proc_.cpu_pct),
                Style::default().fg(cpu_color).bg(row_bg),
            ),
            Span::styled(
                format!("{} ", cpu_spark(app, proc_.pid)),
                Style::default().fg(p::brand()).bg(row_bg),
            ),
            Span::styled(
                format!("{:>5.1} ", mem_pct(proc_.mem_rss, total_mem)),
                Style::default().fg(p::text_primary()).bg(row_bg),
            ),
        ];
        // VIRT only when neither extras are shown.
        if !show_net && !show_gpu {
            spans.push(Span::styled(
                format!("{:>9} ", human_bytes(proc_.mem_virt)),
                Style::default().fg(p::text_muted()).bg(row_bg),
            ));
        }
        spans.push(Span::styled(
            format!(" {:<4} ", proc_.state),
            Style::default()
                .fg(state_color)
                .bg(row_bg)
                .add_modifier(Modifier::BOLD),
        ));
        for rate in [proc_.io_read_rate, proc_.io_write_rate] {
            spans.push(Span::styled(
                format!("{:>10} ", human_rate(rate)),
                Style::default()
                    .fg(if rate > 0.0 {
                        p::brand()
                    } else {
                        p::text_muted()
                    })
                    .bg(row_bg),
            ));
        }
        if show_net {
            let rx = proc_.net_rx_rate.unwrap_or(0.0);
            let tx = proc_.net_tx_rate.unwrap_or(0.0);
            spans.push(Span::styled(
                format!("{:>10} ", human_rate(rx)),
                Style::default()
                    .fg(if rx > 0.0 {
                        p::status_good()
                    } else {
                        p::text_muted()
                    })
                    .bg(row_bg),
            ));
            spans.push(Span::styled(
                format!("{:>10} ", human_rate(tx)),
                Style::default()
                    .fg(if tx > 0.0 {
                        p::tx_rate()
                    } else {
                        p::text_muted()
                    })
                    .bg(row_bg),
            ));
        }
        if show_gpu {
            let pct = proc_.gpu_pct.unwrap_or(0.0);
            spans.push(Span::styled(
                format!("{:>4.0}% ", pct),
                Style::default()
                    .fg(if pct >= 30.0 {
                        p::status_warn()
                    } else if pct > 0.0 {
                        p::brand()
                    } else {
                        p::text_muted()
                    })
                    .bg(row_bg),
            ));
            let mem_str = match proc_.gpu_mem_bytes {
                Some(b) if b > 0 => human_bytes(b),
                _ => "—".into(),
            };
            spans.push(Span::styled(
                format!("{:>9} ", mem_str),
                Style::default()
                    .fg(if proc_.gpu_mem_bytes.is_some() {
                        p::brand()
                    } else {
                        p::text_muted()
                    })
                    .bg(row_bg),
            ));
        }
        spans.push(Span::styled(
            proc_.name.clone(),
            Style::default().fg(p::text_primary()).bg(row_bg),
        ));
        // Trailing fill to extend the SEL_BG band across the row.
        spans.push(Span::styled(
            fill(inner.width as usize, &proc_.name, show_net, show_gpu),
            Style::default().bg(row_bg),
        ));
        let _ = dot_color;
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

fn draw_drill_in(f: &mut Frame, area: Rect, procs: &[ProcTick], sel: usize, total_mem: u64) {
    let Some(p_) = procs.get(sel.min(procs.len().saturating_sub(1))) else {
        let block = panel("DRILL-IN");
        f.render_widget(block, area);
        return;
    };
    let block = panel(&format!("{}  pid {}  -  drill-in", p_.name, p_.pid));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let cmd = if p_.cmd.is_empty() {
        p_.name.clone()
    } else {
        p_.cmd.clone()
    };
    let started = p_
        .start_time
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| {
            chrono::DateTime::<chrono::Local>::from(
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(d.as_secs()),
            )
            .format("%Y-%m-%d %H:%M:%S")
            .to_string()
        })
        .unwrap_or_else(|| "?".into());

    let user_line = format!(
        "{}   ppid {}{}",
        p_.user,
        p_.ppid,
        p_.threads
            .map(|t| format!("   threads {}", t))
            .unwrap_or_default()
    );
    let mem_line = format!(
        "{:.1}%  ({} rss / {} virt){}",
        mem_pct(p_.mem_rss, total_mem),
        human_bytes(p_.mem_rss),
        human_bytes(p_.mem_virt),
        p_.mem_peak
            .map(|b| format!("   peak {}", human_bytes(b)))
            .unwrap_or_default()
    );
    let cpu_line = format!(
        "{:.1}%{}",
        p_.cpu_pct,
        p_.power_w
            .map(|w| format!("   ~{:.2} W", w))
            .unwrap_or_default()
    );
    let io_total = p_.io_read_rate + p_.io_write_rate;
    let net_line = match (p_.net_rx_rate, p_.net_tx_rate) {
        (None, None) => "—".to_string(),
        (rx, tx) => format!(
            "↓ {} / ↑ {}",
            human_rate(rx.unwrap_or(0.0)),
            human_rate(tx.unwrap_or(0.0))
        ),
    };
    let lines = vec![
        kv("cmd", cmd, p::text_primary()),
        kv("user", user_line, p::text_primary()),
        kv("mem", mem_line, p::text_primary()),
        kv("cpu", cpu_line, p::text_primary()),
        kv(
            "io r/w",
            format!(
                "read {} / write {}",
                human_rate(p_.io_read_rate),
                human_rate(p_.io_write_rate)
            ),
            if io_total > 0.0 {
                p::brand()
            } else {
                p::text_muted()
            },
        ),
        kv("net", net_line, p::text_primary()),
        kv("started", started, p::text_muted()),
    ];
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(p::bg())),
        inner,
    );
}

fn kv(k: &str, v: String, val_color: ratatui::style::Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{:<11} ", k), Style::default().fg(p::text_muted())),
        Span::styled(v, Style::default().fg(val_color)),
    ])
}

/// Test-only convenience: same as `filtered_sorted(procs, key, None)`.
#[cfg(test)]
fn sort_procs(procs: &[ProcTick], key: ProcSort) -> Vec<ProcTick> {
    filtered_sorted(procs, key, None)
}

/// 8-cell CPU-usage sparkline for `pid`, right-aligned (newest sample at the
/// right) and left-padded with spaces while the per-pid history is still
/// filling. Empty (all spaces) for pids with no recorded history yet — e.g.
/// while scrubbing a past snapshot whose pids have since exited (issue #10).
fn cpu_spark(app: &App, pid: u32) -> String {
    let width = crate::app::PROC_CPU_SPARK_LEN;
    let samples = app
        .history
        .proc_cpu_history
        .get(&pid)
        .map(|r| r.to_vec())
        .unwrap_or_default();
    let glyphs = crate::ui::widgets::sparkline_string(&samples, app.graph_style);
    let pad = width.saturating_sub(samples.len());
    format!("{}{}", " ".repeat(pad), glyphs)
}

fn fill(width: usize, used: &str, show_net: bool, show_gpu: bool) -> String {
    // Fixed: PID 7+1 + PPID 7+1 + USER 14+1 + %CPU 5+1 + CPU HIST 8+1 + %MEM 5+1
    //        STATE 5+1 + R/s 10+1 + W/s 10+1
    let base = 7 + 1 + 7 + 1 + 14 + 1 + 5 + 1 + 8 + 1 + 5 + 1 + 5 + 1 + 10 + 1 + 10 + 1;
    let virt_w = if !show_net && !show_gpu { 9 + 1 } else { 0 };
    let net_w = if show_net { 10 + 1 + 10 + 1 } else { 0 };
    let gpu_w = if show_gpu { 5 + 1 + 9 + 1 } else { 0 };
    let used_w = base + virt_w + net_w + gpu_w + used.chars().count();
    if width > used_w {
        std::iter::repeat(' ').take(width - used_w).collect()
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
    use std::time::{Duration, SystemTime};

    fn p(pid: u32, name: &str, cpu: f32, rss: u64, io: f64, secs: u64) -> ProcTick {
        ProcTick {
            pid,
            ppid: 1,
            user: "u".into(),
            name: name.into(),
            cmd: name.into(),
            cpu_pct: cpu,
            mem_rss: rss,
            mem_virt: 0,
            threads: None,
            state: 'S',
            start_time: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs)),
            io_read_rate: io,
            ..ProcTick::default()
        }
    }

    fn names(v: &[ProcTick]) -> Vec<&str> {
        v.iter().map(|p| p.name.as_str()).collect()
    }

    fn fixture() -> Vec<ProcTick> {
        vec![
            p(1, "alpha", 5.0, 100, 10.0, 1000),
            p(2, "Bravo", 90.0, 50, 5000.0, 2000),
            p(3, "charlie", 30.0, 9999, 0.0, 500),
            p(4, "delta", 0.5, 200, 20.0, 3000), // newest start
        ]
    }

    #[test]
    fn sort_by_cpu_descending() {
        let s = sort_procs(&fixture(), ProcSort::Cpu);
        assert_eq!(names(&s), vec!["Bravo", "charlie", "alpha", "delta"]);
    }

    #[test]
    fn sort_by_rss_descending() {
        let s = sort_procs(&fixture(), ProcSort::Rss);
        assert_eq!(names(&s), vec!["charlie", "delta", "alpha", "Bravo"]);
    }

    #[test]
    fn sort_by_io_descending() {
        let s = sort_procs(&fixture(), ProcSort::Io);
        assert_eq!(names(&s), vec!["Bravo", "delta", "alpha", "charlie"]);
    }

    #[test]
    fn sort_by_start_newest_first() {
        let s = sort_procs(&fixture(), ProcSort::Start);
        // delta=3000, bravo=2000, alpha=1000, charlie=500
        assert_eq!(names(&s), vec!["delta", "Bravo", "alpha", "charlie"]);
    }

    #[test]
    fn sort_by_name_case_insensitive_ascending() {
        let s = sort_procs(&fixture(), ProcSort::Name);
        // Bravo < alpha lexically (uppercase B < lowercase a) but our sort
        // lowercases first, so the right order is alpha, Bravo, charlie, delta.
        assert_eq!(names(&s), vec!["alpha", "Bravo", "charlie", "delta"]);
    }

    #[test]
    fn sort_empty_is_empty() {
        assert!(sort_procs(&[], ProcSort::Cpu).is_empty());
        assert!(sort_procs(&[], ProcSort::Name).is_empty());
    }

    #[test]
    fn sort_does_not_mutate_input() {
        let input = fixture();
        let original_first = input[0].name.clone();
        let _ = sort_procs(&input, ProcSort::Cpu);
        assert_eq!(input[0].name, original_first);
    }

    // ── Gpu / Net sort ──────────────────────────────────────────────────

    fn fixture_with_gpu_net() -> Vec<ProcTick> {
        let mut v = fixture();
        // alpha: 0% GPU, no net
        // Bravo: 95% GPU, 1MB/s net
        v[1].gpu_pct = Some(95.0);
        v[1].net_rx_rate = Some(800_000.0);
        v[1].net_tx_rate = Some(200_000.0);
        // charlie: 5% GPU, 200KB/s net
        v[2].gpu_pct = Some(5.0);
        v[2].net_rx_rate = Some(50_000.0);
        v[2].net_tx_rate = Some(150_000.0);
        // delta: no GPU/net data
        v
    }

    #[test]
    fn sort_by_gpu_descending_with_none_at_bottom() {
        let s = sort_procs(&fixture_with_gpu_net(), ProcSort::Gpu);
        // Bravo (95) > charlie (5) > {alpha, delta} (None → 0; their
        // relative order is whatever the sort happens to produce, but
        // they must come after the procs with values).
        assert_eq!(s[0].name, "Bravo");
        assert_eq!(s[1].name, "charlie");
        let tail: Vec<&str> = s[2..].iter().map(|p| p.name.as_str()).collect();
        assert!(tail.contains(&"alpha"));
        assert!(tail.contains(&"delta"));
    }

    #[test]
    fn sort_by_net_uses_combined_rx_plus_tx() {
        let s = sort_procs(&fixture_with_gpu_net(), ProcSort::Net);
        // Bravo total = 1_000_000; charlie = 200_000; alpha/delta = 0.
        assert_eq!(s[0].name, "Bravo");
        assert_eq!(s[1].name, "charlie");
    }

    #[test]
    fn proc_sort_cycle_visits_gpu_and_net() {
        // ProcSort::ALL must include the new variants.
        let labels: Vec<&str> = ProcSort::ALL.iter().map(|s| s.label()).collect();
        assert!(labels.contains(&"gpu"));
        assert!(labels.contains(&"net"));
    }

    // ── filter behavior ─────────────────────────────────────────────────

    #[test]
    fn filter_none_returns_full_sorted_list() {
        let s = filtered_sorted(&fixture(), ProcSort::Cpu, None);
        assert_eq!(s.len(), 4);
    }

    #[test]
    fn filter_substring_is_case_insensitive() {
        // "BRAV" should still match "Bravo"; "Char" matches "charlie".
        let a = filtered_sorted(&fixture(), ProcSort::Cpu, Some("BRAV"));
        assert_eq!(names(&a), vec!["Bravo"]);
        let b = filtered_sorted(&fixture(), ProcSort::Cpu, Some("Char"));
        assert_eq!(names(&b), vec!["charlie"]);
    }

    #[test]
    fn filter_no_match_returns_empty() {
        let s = filtered_sorted(&fixture(), ProcSort::Cpu, Some("zzz_no_proc"));
        assert!(s.is_empty());
    }

    #[test]
    fn filter_then_sort_preserves_sort_order() {
        // Two procs match "a" — alpha and charlie — and Cpu sort should
        // put charlie (30) before alpha (5).
        let s = filtered_sorted(&fixture(), ProcSort::Cpu, Some("a"));
        // Hmm — "Bravo" and "delta" also contain 'a', so all 4 match.
        // Sort by CPU desc: Bravo 90, charlie 30, alpha 5, delta 0.5.
        assert_eq!(names(&s), vec!["Bravo", "charlie", "alpha", "delta"]);
    }

    #[test]
    fn filter_matches_user_field() {
        let mut procs = fixture();
        procs[0].user = "deploy".into();
        let s = filtered_sorted(&procs, ProcSort::Cpu, Some("deploy"));
        assert_eq!(names(&s), vec!["alpha"]);
    }

    #[test]
    fn filter_matches_cmd_field() {
        let mut procs = fixture();
        procs[1].cmd = "/usr/bin/some-binary --flag".into();
        let s = filtered_sorted(&procs, ProcSort::Cpu, Some("--flag"));
        assert_eq!(names(&s), vec!["Bravo"]);
    }
}
