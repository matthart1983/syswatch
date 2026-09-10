//! Bounded subprocess execution for the collectors that shell out.
//!
//! Every external command syswatch runs (`systemctl`, `launchctl`,
//! `ioreg`, `pmset`, `ss`, `lsof`, `nettop`, `system_profiler`) is
//! something that can wedge: dbus down, systemd inside a container,
//! IOKit busy. `Command::output()` would then block the caller for as
//! long as the child hangs. Everything goes through `run_with_timeout`
//! instead, which kills the child at the deadline and returns `None` so
//! the collector keeps its cached value.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Budget for the periodic collectors (services, power, GPU stats,
/// per-process bandwidth). Anything slower than this on a healthy host
/// is already too slow to sample every few seconds.
pub const PERIODIC_TIMEOUT: Duration = Duration::from_millis(1500);

/// Budget for one-shot discovery at startup (`system_profiler`), which
/// is legitimately slow on some Macs.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);

/// Run `program` with `args`, capturing stdout. Returns `None` if the
/// program cannot be spawned, exits with an error we cannot read, or
/// outlives `timeout` (in which case it is killed and reaped).
///
/// On timeout the reader thread is *not* joined: a grandchild that
/// inherited the stdout pipe (`sh -c 'sleep 30'` leaves `sleep` alive
/// after `sh` is killed) keeps the write end open, and joining would
/// wait for it. The reader exits on its own once the pipe closes.
pub fn run_with_timeout(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });

    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let _ = child.wait();
                return reader.join().ok();
            }
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                drop(reader);
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                drop(reader);
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_stdout_of_a_fast_command() {
        let out = run_with_timeout("echo", &["hello"], Duration::from_secs(5));
        assert_eq!(out.as_deref().map(str::trim), Some("hello"));
    }

    #[test]
    fn missing_program_is_none() {
        assert!(run_with_timeout("syswatch-no-such-binary", &[], Duration::from_secs(1)).is_none());
    }

    #[test]
    fn grandchild_holding_the_pipe_does_not_extend_the_deadline() {
        // `sh` is killed at the deadline but its `sleep` child inherits
        // stdout and lives on; the call must still return promptly.
        let started = Instant::now();
        let out = run_with_timeout(
            "sh",
            &["-c", "sleep 30; echo never"],
            Duration::from_millis(300),
        );
        assert!(out.is_none());
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "took {:?}, waited on the grandchild's pipe",
            started.elapsed()
        );
    }

    #[test]
    fn hung_command_is_killed_at_the_deadline() {
        let started = Instant::now();
        let out = run_with_timeout("sleep", &["30"], Duration::from_millis(300));
        assert!(out.is_none());
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "took {:?}, the child was not killed",
            started.elapsed()
        );
    }
}
