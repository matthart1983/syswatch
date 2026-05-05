use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{App, ProcSort, Snapshot};
use crate::collect::ProcTick;
use crate::ui::{
    palette as p,
    widgets::{human_bytes, human_rate, panel},
};

pub fn draw(f: &mut Frame, area: Rect, app: &App, snap: &Snapshot) {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // sort strip
            Constraint::Min(0),    // process table
            Constraint::Length(7), // drill-in
        ])
        .split(area);

    draw_sort_strip(f, v[0], app, snap);
    let sorted = sort_procs(&snap.procs, app.proc_sort);
    draw_table(f, v[1], app, &sorted);
    draw_drill_in(f, v[2], &sorted, app.proc_sel);
}

fn draw_sort_strip(f: &mut Frame, area: Rect, app: &App, snap: &Snapshot) {
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
    spans.push(Span::styled(
        format!(
            "    {} procs   press s to cycle sort, ↑/↓ select",
            snap.procs.len()
        ),
        Style::default().fg(p::text_muted()),
    ));
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(p::bg())),
        area,
    );
}

fn draw_table(f: &mut Frame, area: Rect, app: &App, procs: &[ProcTick]) {
    let block = panel("PROCESSES");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let header = Line::from(vec![
        Span::styled(format!("{:>7} ", "PID"), header_style()),
        Span::styled(format!("{:>7} ", "PPID"), header_style()),
        Span::styled(format!("{:<14} ", "USER"), header_style()),
        Span::styled(format!("{:>6} ", "%CPU"), header_style()),
        Span::styled(format!("{:>9} ", "RSS"), header_style()),
        Span::styled(format!("{:>9} ", "VIRT"), header_style()),
        Span::styled(format!("{:<5} ", "STATE"), header_style()),
        Span::styled(format!("{:>11} ", "IO/s"), header_style()),
        Span::styled("COMMAND", header_style()),
    ]);

    let take = inner.height.saturating_sub(1) as usize;
    let sel_clamped = app.proc_sel.min(procs.len().saturating_sub(1));
    // Scroll: keep selection visible.
    let start = sel_clamped.saturating_sub(take.saturating_sub(1));
    let end = (start + take).min(procs.len());

    let mut lines = vec![header];
    for (i, proc_) in procs[start..end].iter().enumerate() {
        let abs = start + i;
        let selected = abs == sel_clamped;
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
        let spans = vec![
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
                format!("{:>9} ", human_bytes(proc_.mem_rss)),
                Style::default().fg(p::text_primary()).bg(row_bg),
            ),
            Span::styled(
                format!("{:>9} ", human_bytes(proc_.mem_virt)),
                Style::default().fg(p::text_muted()).bg(row_bg),
            ),
            Span::styled(
                format!(" {:<4} ", proc_.state),
                Style::default()
                    .fg(state_color)
                    .bg(row_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{:>11} ", human_rate(proc_.io_rate)),
                Style::default()
                    .fg(if proc_.io_rate > 0.0 {
                        p::brand()
                    } else {
                        p::text_muted()
                    })
                    .bg(row_bg),
            ),
            Span::styled(
                proc_.name.clone(),
                Style::default().fg(p::text_primary()).bg(row_bg),
            ),
            // Trailing fill to extend the SEL_BG band across the row.
            Span::styled(
                fill(inner.width as usize, &proc_.name),
                Style::default().bg(row_bg),
            ),
            // Status dot at the very start? No — append a leading dot replaces alignment. Skip.
            Span::raw(""),
        ];
        let _ = dot_color;
        lines.push(Line::from(spans));
    }

    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(p::bg())),
        inner,
    );
}

fn draw_drill_in(f: &mut Frame, area: Rect, procs: &[ProcTick], sel: usize) {
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

    let lines = vec![
        kv("cmd", cmd, p::text_primary()),
        kv("ppid", p_.ppid.to_string(), p::text_primary()),
        kv("user", p_.user.clone(), p::text_primary()),
        kv(
            "rss / virt",
            format!("{} / {}", human_bytes(p_.mem_rss), human_bytes(p_.mem_virt)),
            p::text_primary(),
        ),
        kv("cpu", format!("{:.1}%", p_.cpu_pct), p::text_primary()),
        kv(
            "io rate",
            human_rate(p_.io_rate),
            if p_.io_rate > 0.0 {
                p::brand()
            } else {
                p::text_muted()
            },
        ),
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

fn sort_procs(procs: &[ProcTick], key: ProcSort) -> Vec<ProcTick> {
    let mut out = procs.to_vec();
    match key {
        ProcSort::Cpu => out.sort_by(|a, b| {
            b.cpu_pct
                .partial_cmp(&a.cpu_pct)
                .unwrap_or(std::cmp::Ordering::Equal)
        }),
        ProcSort::Rss => out.sort_by(|a, b| b.mem_rss.cmp(&a.mem_rss)),
        ProcSort::Io => out.sort_by(|a, b| {
            b.io_rate
                .partial_cmp(&a.io_rate)
                .unwrap_or(std::cmp::Ordering::Equal)
        }),
        ProcSort::Start => out.sort_by(|a, b| b.start_time.cmp(&a.start_time)),
        ProcSort::Name => out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
    }
    out
}

fn fill(width: usize, used: &str) -> String {
    // 7+1 + 7+1 + 14+1 + 5+1 + 9+1 + 9+1 + 5+1 + 11+1 = 73
    let used_w = 73 + used.chars().count();
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
            threads: 1,
            state: 'S',
            start_time: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs)),
            io_rate: io,
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
}
