//! The collector thread.
//!
//! `Collector::sample` does real work — a sysinfo process refresh,
//! `/proc` and sysfs reads, and on a budget `ss`, `systemctl`, `ioreg`
//! and friends. Running it on the UI thread meant every one of those
//! stalled input and rendering for its duration (issue #2). The
//! `Collector` now lives on its own thread and hands finished
//! `Snapshot`s to the UI over a channel; the UI never waits on it.
//!
//! The channel holds one snapshot. If the UI falls behind, the newest
//! sample replaces the one it has not read yet rather than queueing.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::collector::Collector;
use super::model::Snapshot;

/// Knobs the UI thread can turn while the worker runs.
struct Control {
    tick_ms: AtomicU64,
    paused: AtomicBool,
    stop: AtomicBool,
}

/// UI-side handle to the collector thread.
pub struct CollectorHandle {
    rx: Receiver<Snapshot>,
    ctrl: Arc<Control>,
    join: Option<JoinHandle<()>>,
}

impl CollectorHandle {
    /// Spawn the collector thread. The `Collector` is constructed on
    /// the worker so none of its platform handles ever cross threads.
    /// The first sample is taken immediately.
    pub fn spawn(tick_ms: u64) -> Self {
        let (tx, rx) = mpsc::sync_channel::<Snapshot>(1);
        let ctrl = Arc::new(Control {
            tick_ms: AtomicU64::new(tick_ms),
            paused: AtomicBool::new(false),
            stop: AtomicBool::new(false),
        });
        let worker_ctrl = Arc::clone(&ctrl);
        let join = std::thread::Builder::new()
            .name("syswatch-collector".into())
            .spawn(move || run_loop(tx, worker_ctrl, tick_ms))
            .expect("spawn collector thread");
        Self {
            rx,
            ctrl,
            join: Some(join),
        }
    }

    /// Take every snapshot the worker has produced since the last call,
    /// oldest first. Never blocks.
    pub fn drain(&self) -> Vec<Snapshot> {
        let mut out = Vec::new();
        while let Ok(s) = self.rx.try_recv() {
            out.push(s);
        }
        out
    }

    /// Update the sample interval. Takes effect from the next tick.
    pub fn set_tick_ms(&self, tick_ms: u64) {
        self.ctrl.tick_ms.store(tick_ms, Ordering::Relaxed);
    }

    /// While paused the worker sleeps instead of sampling, so a paused
    /// screen costs nothing and no snapshots pile up behind it.
    pub fn set_paused(&self, paused: bool) {
        self.ctrl.paused.store(paused, Ordering::Relaxed);
    }
}

impl Drop for CollectorHandle {
    fn drop(&mut self) {
        self.ctrl.stop.store(true, Ordering::Relaxed);
        // Do not join: a subprocess inside `sample()` is bounded by
        // `command::PERIODIC_TIMEOUT`, but quitting should never wait
        // for it. The thread dies with the process.
        let _ = self.join.take();
    }
}

fn run_loop(tx: SyncSender<Snapshot>, ctrl: Arc<Control>, tick_ms: u64) {
    let mut collector = Collector::new(tick_ms);
    // Poll the control flags at this granularity so pause, tick
    // changes and quit are picked up promptly without busy-waiting.
    const SLICE: Duration = Duration::from_millis(50);
    let mut next_sample = Instant::now();
    loop {
        if ctrl.stop.load(Ordering::Relaxed) {
            return;
        }
        let now = Instant::now();
        if now < next_sample || ctrl.paused.load(Ordering::Relaxed) {
            std::thread::sleep(SLICE.min(next_sample.saturating_duration_since(now).max(SLICE)));
            continue;
        }
        let snap = collector.sample();
        match tx.try_send(snap) {
            Ok(()) => {}
            Err(TrySendError::Full(snap)) => {
                // UI has not consumed the previous one; replace it with
                // the fresher sample rather than block the collector.
                let _ = tx.try_send(snap);
            }
            Err(TrySendError::Disconnected(_)) => return,
        }
        let tick = Duration::from_millis(ctrl.tick_ms.load(Ordering::Relaxed).clamp(100, 5000));
        next_sample = Instant::now() + tick;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_delivers_a_first_snapshot_promptly() {
        let handle = CollectorHandle::spawn(1000);
        let started = Instant::now();
        let mut got = Vec::new();
        while got.is_empty() && started.elapsed() < Duration::from_secs(10) {
            got = handle.drain();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(got.len(), 1, "expected exactly one initial snapshot");
        assert!(!got[0].cpu.per_core.is_empty() || got[0].host.cpu_cores > 0);
    }

    #[test]
    fn paused_worker_stops_producing() {
        let handle = CollectorHandle::spawn(100);
        // Wait for the first sample, then pause.
        let started = Instant::now();
        while handle.drain().is_empty() && started.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        handle.set_paused(true);
        // Anything already in flight lands within one slice.
        std::thread::sleep(Duration::from_millis(200));
        handle.drain();
        std::thread::sleep(Duration::from_millis(400));
        assert!(handle.drain().is_empty(), "paused worker kept sampling");
    }
}
