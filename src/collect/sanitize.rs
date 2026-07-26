//! Terminal-safety scrubbing for collector-sourced strings.
//!
//! Process names, mount paths, device names and service labels are all
//! attacker-controllable by any unprivileged local user — and syswatch is
//! frequently run under sudo. ratatui does *not* protect us here: only the
//! legacy `Buffer::set_stringn` filters control characters, while the paths
//! we actually use (`Line`/`Span` for table rows, `Paragraph` for panels)
//! write the grapheme straight into the cell buffer, which the crossterm
//! backend then emits verbatim. A process named `sh\x1b]0;owned\x07` would
//! retitle the viewer's terminal; the same trick reaches screen-clear,
//! cursor moves and OSC 52 clipboard writes.
//!
//! So we scrub once, at the boundary where a `Snapshot` enters the process —
//! `Collector::sample` for live data, `recording::read` for replay — rather
//! than at each of the ~40 render sites. Everything downstream (TUI, JSON
//! snapshot dump, recording file) is then safe by construction.
//!
//! Issue #21.

use crate::collect::model::Snapshot;

/// Stand-in for a character that must never reach the terminal. Matches
/// netwatch's convention in its packet ASCII pane.
const REPLACEMENT: char = '·';

/// True for characters that can move the cursor, start an escape
/// sequence, or reorder the visible line.
fn unsafe_for_display(c: char) -> bool {
    // C0 (incl. ESC/BEL/TAB/CR/LF), DEL, and C1 (incl. the 8-bit CSI
    // U+009B, which xterm-family terminals honour as readily as ESC-[).
    c.is_control()
        // Bidirectional overrides — "trojan source". These render at zero
        // width so ratatui drops them, but they survive into the JSON
        // snapshot and recording, where they can still make a malicious
        // process name read as a benign one. Same set rustc lints on.
        || matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Replace unsafe characters in place. Allocates only when the string
/// actually needs scrubbing, which is the overwhelmingly common case
/// (every proc name on a healthy box is clean).
fn scrub(s: &mut String) {
    if s.chars().any(unsafe_for_display) {
        *s = s
            .chars()
            .map(|c| {
                if unsafe_for_display(c) {
                    REPLACEMENT
                } else {
                    c
                }
            })
            .collect();
    }
}

fn scrub_opt(s: &mut Option<String>) {
    if let Some(s) = s.as_mut() {
        scrub(s);
    }
}

/// Scrub every externally-sourced string in a snapshot.
///
/// When adding a `String` field to a model struct, add it here too — the
/// `every_string_field_is_scrubbed` test will catch the omission for the
/// fields it knows about, but it can't see fields that don't exist yet.
pub fn scrub_snapshot(snap: &mut Snapshot) {
    scrub(&mut snap.host.hostname);
    scrub(&mut snap.host.os);
    scrub(&mut snap.host.cpu_model);

    for d in &mut snap.disks {
        scrub(&mut d.mount_point);
        scrub(&mut d.device);
        scrub(&mut d.fs_type);
    }

    for n in &mut snap.net {
        scrub(&mut n.name);
    }

    for p in &mut snap.procs {
        scrub(&mut p.user);
        scrub(&mut p.name);
        scrub(&mut p.cmd);
    }

    for g in &mut snap.gpus {
        scrub(&mut g.name);
        scrub(&mut g.vendor);
        scrub_opt(&mut g.driver);
        scrub_opt(&mut g.live_data_hint);
    }

    for z in &mut snap.power.thermal_zones {
        scrub(&mut z.name);
    }
    for f in &mut snap.power.fans {
        scrub(&mut f.name);
    }
    scrub_opt(&mut snap.power.live_data_hint);

    for s in &mut snap.services {
        scrub(&mut s.name);
        scrub(&mut s.detail);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::model::{
        DiskUsageTick, FanTick, GpuTick, InterfaceTick, ProcTick, ServiceTick, ThermalZone,
    };

    /// The sequence that motivated the issue: OSC 0 retitles the terminal,
    /// BEL terminates it. Neither byte may survive.
    #[test]
    fn strips_osc_terminal_title_sequence() {
        let mut s = String::from("sh\u{1b}]0;owned\u{7}x");
        scrub(&mut s);
        assert_eq!(s, "sh·]0;owned·x");
    }

    #[test]
    fn strips_c0_c1_and_del() {
        for c in ['\u{1b}', '\u{7}', '\t', '\n', '\r', '\u{7f}', '\u{9b}'] {
            let mut s = format!("a{c}b");
            scrub(&mut s);
            assert_eq!(s, "a·b", "U+{:04X} survived", c as u32);
        }
    }

    #[test]
    fn strips_bidi_overrides() {
        // "gnp.exe" spelled to render as "exe.png" under an RLO.
        let mut s = String::from("gnp\u{202E}.exe");
        scrub(&mut s);
        assert_eq!(s, "gnp·.exe");
    }

    #[test]
    fn leaves_ordinary_text_untouched() {
        for input in [
            "kernel_task",
            "/System/Volumes/Data",
            "日本語",
            "café",
            "🦀",
        ] {
            let mut s = String::from(input);
            scrub(&mut s);
            assert_eq!(s, input);
        }
    }

    /// Combining marks and ZWJ emoji sequences are legitimate text —
    /// scrubbing them would mangle real filenames.
    #[test]
    fn leaves_combining_marks_and_zwj_untouched() {
        let mut s = String::from("e\u{301} \u{1F468}\u{200D}\u{1F4BB}");
        let before = s.clone();
        scrub(&mut s);
        assert_eq!(s, before);
    }

    #[test]
    fn every_string_field_is_scrubbed() {
        let evil = "x\u{1b}[2Jy";
        let clean = "x·[2Jy";

        let mut snap = Snapshot {
            disks: vec![DiskUsageTick {
                mount_point: evil.into(),
                device: evil.into(),
                fs_type: evil.into(),
                ..Default::default()
            }],
            net: vec![InterfaceTick {
                name: evil.into(),
                ..Default::default()
            }],
            procs: vec![ProcTick {
                user: evil.into(),
                name: evil.into(),
                cmd: evil.into(),
                ..Default::default()
            }],
            gpus: vec![GpuTick {
                name: evil.into(),
                vendor: evil.into(),
                driver: Some(evil.into()),
                live_data_hint: Some(evil.into()),
                ..Default::default()
            }],
            services: vec![ServiceTick {
                name: evil.into(),
                detail: evil.into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        snap.host.hostname = evil.into();
        snap.host.os = evil.into();
        snap.host.cpu_model = evil.into();
        snap.power.thermal_zones = vec![ThermalZone {
            name: evil.into(),
            temp_c: 0.0,
        }];
        snap.power.fans = vec![FanTick {
            name: evil.into(),
            rpm: 0,
            target_rpm: None,
        }];
        snap.power.live_data_hint = Some(evil.into());

        scrub_snapshot(&mut snap);

        // Serialize the whole snapshot and assert no ESC survives anywhere —
        // catches fields the explicit walk forgot, as long as they're in
        // the JSON. (serde_json escapes control chars as \u001b, so we
        // check the escaped form too.)
        let json = serde_json::to_string(&snap).expect("serialize");
        assert!(!json.contains('\u{1b}'), "raw ESC survived");
        assert!(!json.contains("\\u001b"), "escaped ESC survived");
        assert!(json.contains(clean), "expected scrubbed text in output");
    }
}
