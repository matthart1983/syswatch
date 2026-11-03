//! `syswatch --record` has to keep every tick it sampled when something
//! stops it. Its help promises that Ctrl-C and SIGTERM flush the current
//! chunk, `systemctl stop` sends SIGTERM, and a closed terminal sends
//! SIGHUP. The recorder buffers up to 60 ticks in memory before it writes
//! a block, so a stop that skips the flush loses them.
//!
//! There is no `[lib]` target, so these tests drive the built binary.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_syswatch");

/// Starting the collector can take seconds on a loaded machine, and the
/// first chunk file only appears after that. Generous, because it's polled.
const START_DEADLINE: Duration = Duration::from_secs(120);
/// From the signal to exit is one sample at most, plus a 200 ms stop poll.
const EXIT_DEADLINE: Duration = Duration::from_secs(60);

/// A running recorder with its own HOME and XDG directories. Killed and
/// reaped on drop, so a failed assertion doesn't leave it sampling.
struct Recorder {
    child: Child,
    home: tempfile::TempDir,
}

impl Recorder {
    fn start(configure: impl FnOnce(&mut Command)) -> Self {
        let home = tempfile::tempdir().expect("temp dir");
        let h = home.path();
        let mut cmd = Command::new(BIN);
        cmd.args(["--record", "--keep", "1h", "--tick", "100"])
            .env("HOME", h)
            .env("XDG_CONFIG_HOME", h.join("config"))
            .env("XDG_DATA_HOME", h.join("data"))
            .env("XDG_STATE_HOME", h.join("state"))
            .env("XDG_CACHE_HOME", h.join("cache"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure(&mut cmd);
        let child = cmd.spawn().expect("spawn syswatch --record");
        Recorder { child, home }
    }

    /// Wait for the first chunk file. `run_ring` creates it just before
    /// taking a sample and only checks for a stop request between samples,
    /// so once it exists at least one tick will be pushed, whenever a
    /// signal lands.
    fn wait_for_chunk(&mut self) -> PathBuf {
        let child = &mut self.child;
        let home = self.home.path();
        poll(START_DEADLINE, "the first chunk file", || {
            if let Some(status) = child.try_wait().expect("try_wait") {
                panic!("recorder exited before writing a chunk: {status}");
            }
            chunks(home).into_iter().next()
        })
    }

    fn signal(&self, sig: libc::c_int) {
        // SAFETY: plain kill(2) on our own child's pid.
        let rc = unsafe { libc::kill(self.child.id() as libc::pid_t, sig) };
        assert_eq!(rc, 0, "kill: {}", std::io::Error::last_os_error());
    }

    fn wait_for_exit(&mut self) -> ExitStatus {
        let child = &mut self.child;
        poll(EXIT_DEADLINE, "the recorder to exit", || {
            child.try_wait().expect("try_wait")
        })
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn poll<T>(deadline: Duration, what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let end = Instant::now() + deadline;
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(
            Instant::now() < end,
            "gave up after {deadline:?} waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Every `.swr` file under `dir`. Searched rather than built from the XDG
/// path, because on macOS the data directory hangs off HOME instead.
fn chunks(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "swr") {
                found.push(p);
            }
        }
    }
    found
}

/// Ticks in a v3 `.swr` file, counted from its framing: a 6-byte header,
/// then blocks of `u32` length plus zstd data, each decompressing to
/// records of `u32` length plus postcard bytes. Panics on a short header,
/// a partial block or a record that overruns its block, so a torn write
/// can't pass as a short recording.
fn count_ticks(path: &Path) -> usize {
    let data = fs::read(path).expect("read chunk");
    assert!(
        data.len() >= 6,
        "{} is {} bytes, short of the 6-byte header: nothing was flushed",
        path.display(),
        data.len()
    );
    assert_eq!(&data[..4], b"SWR\0", "bad magic in {}", path.display());
    let u32_at = |b: &[u8], at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
    let mut pos = 6;
    let mut ticks = 0;
    while pos < data.len() {
        assert!(pos + 4 <= data.len(), "partial block length at byte {pos}");
        let len = u32_at(&data, pos) as usize;
        pos += 4;
        assert!(
            pos + len <= data.len(),
            "block at byte {} runs past the end of the file",
            pos - 4
        );
        let block = zstd::stream::decode_all(&data[pos..pos + len]).expect("block decompresses");
        pos += len;
        let mut at = 0;
        while at < block.len() {
            assert!(at + 4 <= block.len(), "partial record length in a block");
            at += 4 + u32_at(&block, at) as usize;
            assert!(at <= block.len(), "record runs past the end of its block");
            ticks += 1;
        }
    }
    ticks
}

/// Record until the first chunk exists, stop the recorder with `sig`, and
/// check it exits through its own shutdown path with its ticks on disk.
fn stop_with(sig: libc::c_int, name: &str) {
    let mut rec = Recorder::start(|_| {});
    let chunk = rec.wait_for_chunk();
    // Let a few ticks pile up in the unflushed block so the stop has
    // something to lose. The assertions hold however many there are.
    std::thread::sleep(Duration::from_millis(500));
    rec.signal(sig);
    let status = rec.wait_for_exit();

    assert!(
        status.success(),
        "{name} should stop the recorder cleanly so it flushes, got {status} \
         with the chunk at {} bytes",
        fs::metadata(&chunk).map(|m| m.len()).unwrap_or(0)
    );
    assert_eq!(
        chunks(rec.home.path()),
        vec![chunk.clone()],
        "expected exactly one chunk"
    );
    let ticks = count_ticks(&chunk);
    assert!(ticks >= 1, "{name} left {ticks} ticks in the chunk");
}

#[test]
fn sigterm_flushes_the_recording() {
    stop_with(libc::SIGTERM, "SIGTERM");
}

#[test]
fn sighup_flushes_the_recording() {
    stop_with(libc::SIGHUP, "SIGHUP");
}

#[test]
fn sigint_flushes_the_recording() {
    stop_with(libc::SIGINT, "SIGINT");
}

/// `nohup` starts a command with SIGHUP ignored so it outlives the
/// terminal, and the docs suggest running `--record` that way. The stop
/// handler must not replace an ignore the recorder inherited, or closing
/// the terminal would end a `nohup` recording.
///
/// Linux only, because it reads the dispositions from `/proc`. Checking
/// them directly avoids guessing how long "still running" has to last.
#[cfg(target_os = "linux")]
#[test]
fn a_hangup_ignored_at_launch_stays_ignored() {
    use std::os::unix::process::CommandExt;

    let mut rec = Recorder::start(|cmd| {
        // SAFETY: signal(2) is async-signal-safe, so it may run between
        // fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                libc::signal(libc::SIGHUP, libc::SIG_IGN);
                Ok(())
            });
        }
    });
    // The handler is installed before the first chunk is created.
    let chunk = rec.wait_for_chunk();

    let status = fs::read_to_string(format!("/proc/{}/status", rec.child.id())).expect("status");
    let mask = |field: &str| {
        let line = status
            .lines()
            .find(|l| l.starts_with(field))
            .unwrap_or_else(|| panic!("no {field} in /proc status"));
        u64::from_str_radix(line[field.len()..].trim(), 16).expect("hex mask")
    };
    let bit = |sig: libc::c_int| 1u64 << (sig - 1);
    assert_ne!(
        mask("SigIgn:") & bit(libc::SIGHUP),
        0,
        "SIGHUP was ignored at launch and should still be"
    );
    assert_ne!(
        mask("SigCgt:") & bit(libc::SIGTERM),
        0,
        "SIGTERM should still be caught"
    );

    rec.signal(libc::SIGHUP);
    rec.signal(libc::SIGTERM);
    let status = rec.wait_for_exit();
    assert!(
        status.success(),
        "SIGTERM after an ignored SIGHUP: {status}"
    );
    assert!(count_ticks(&chunk) >= 1);
}
