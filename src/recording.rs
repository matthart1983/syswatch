//! Session recording (.swr files).
//!
//! When the user presses `R`, syswatch starts appending each tick's
//! `Snapshot` to a binary file under the local data dir. Pressing `R`
//! again (or quitting) flushes and closes. The file is then replayable
//! via `syswatch --replay path/to/session.swr`.
//!
//! ## Format (v3)
//!
//! A 6-byte header (`b"SWR\0" + u16 version`) followed by a sequence of
//! zstd-compressed blocks, each `u32 length` (LE) + `length` bytes of
//! compressed data. Decompressed, a block is itself a sequence of
//! length-prefixed postcard-encoded [`EncodedTick`] records — the same
//! `u32 len + bytes` framing v1/v2 used directly on the file, just one
//! level deeper now. [`Recorder`] buffers [`BLOCK_TICKS`] ticks before
//! compressing and flushing a block; [`Drop`] flushes whatever's
//! buffered so quitting mid-block doesn't lose it.
//!
//! `EncodedTick` is `Snapshot` with two changes that were most of a
//! v2 recording's size on a typical box:
//!
//! - **Process identity is recorded once per pid, not once per tick.**
//!   A process's name, command line, user, parent pid and start time
//!   don't change after it starts, so v2 spent most of its bytes
//!   resending three strings (name/cmd/user) for every one of a few
//!   hundred processes, every tick, for the life of the recording.
//!   [`ProcIdentity`] carries those fields and is only emitted the
//!   first time a pid is seen, or again if its identity changes — pid
//!   reuse means "have we seen this pid" isn't the right test, so the
//!   comparison is on the identity itself (`start_time` in particular
//!   is effectively unique per process instance). Everything that
//!   actually changes tick to tick ([`ProcVolatile`]) is still sent
//!   every tick, in the snapshot's original order, so decode rebuilds
//!   `Snapshot::procs` exactly as captured.
//! - **An unchanged services list isn't resent.** `ServicesCollector`
//!   already caches its subprocess output for several seconds, so
//!   consecutive ticks routinely carry byte-identical `services`
//!   vectors. `EncodedTick::services` is `None` when it matches the
//!   previous tick's, `Some(_)` otherwise (including the first tick).
//!
//! No periodic keyframe is needed for either scheme: replay is a
//! sequential decode from byte 0, never a random seek into the middle
//! of the file, so there's nothing to reset a keyframe *for*. The
//! existing truncated/corrupt-tail tolerance carries over unchanged —
//! [`RecordingReader`] is a plain iterator that stops (rather than
//! erroring) at the first bad record or block, so a recording cut off
//! by a crash mid-tick still replays everything before that point.
//!
//! v1 and v2 files are rejected with a clear error, same as v2
//! rejected v1 — postcard isn't self-describing and the encoding
//! changed shape, so there's no way to read the old bytes as the new
//! format.

use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

use crate::collect::model::{
    CpuTick, DiskIoTick, DiskUsageTick, GpuTick, HostInfo, InterfaceTick, MemTick, PowerTick,
    PressureTick, ProcTick, ServiceTick,
};
use crate::collect::Snapshot;

/// Magic bytes at the start of every .swr file. ASCII so `file(1)`
/// can identify them later if we add a magic database entry.
pub const MAGIC: &[u8; 4] = b"SWR\0";
/// Bumped on incompatible format changes. postcard isn't
/// self-describing, so any change to the on-disk shape is incompatible
/// both ways — replay refuses any version other than its own with a
/// clear error rather than silently returning zero (or garbage)
/// snapshots. v3: process-table identity/volatile split + services
/// dedup + zstd block compression (see module docs).
pub const FORMAT_VERSION: u16 = 3;

/// Ticks buffered per compressed block. Small enough that `R` then a
/// quick `R` to stop doesn't lose much to an unflushed partial block;
/// large enough that zstd has a useful amount of cross-tick redundancy
/// to work with (mostly repeated zeros and small numbers in
/// `ProcVolatile` for the long tail of near-idle processes).
const BLOCK_TICKS: usize = 60;
const ZSTD_LEVEL: i32 = 3;

/// Local data dir for session files.
pub fn dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join("syswatch").join("sessions"))
}

/// Build a fresh, filesystem-safe session path with a wall-clock stamp.
pub fn fresh_path() -> Option<PathBuf> {
    let dir = dir()?;
    let ts: DateTime<Local> = std::time::SystemTime::now().into();
    Some(dir.join(format!("session-{}.swr", ts.format("%Y-%m-%dT%H-%M-%S"))))
}

// ── On-disk shapes ──────────────────────────────────────────────────────

/// Fields that are genuinely constant for a process's lifetime — it
/// doesn't rename itself, change its command line, or get re-parented
/// after exec. Recorded once per pid (or again if the identity itself
/// changes, which is how pid reuse is detected: see `IdentityTracker`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ProcIdentity {
    pid: u32,
    ppid: u32,
    user: String,
    name: String,
    cmd: String,
    start_time: Option<SystemTime>,
}

impl ProcIdentity {
    fn from_proc(p: &ProcTick) -> Self {
        Self {
            pid: p.pid,
            ppid: p.ppid,
            user: p.user.clone(),
            name: p.name.clone(),
            cmd: p.cmd.clone(),
            start_time: p.start_time,
        }
    }
}

/// Everything about a process that can change tick to tick.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProcVolatile {
    pid: u32,
    cpu_pct: f32,
    mem_rss: u64,
    mem_virt: u64,
    threads: Option<u32>,
    state: char,
    io_read_rate: f64,
    io_write_rate: f64,
    net_rx_rate: Option<f64>,
    net_tx_rate: Option<f64>,
    gpu_pct: Option<f32>,
    gpu_mem_bytes: Option<u64>,
    mem_footprint: Option<u64>,
    mem_pss: Option<u64>,
    mem_private: Option<u64>,
    mem_shared: Option<u64>,
    mem_swap: Option<u64>,
    mem_peak: Option<u64>,
    power_w: Option<f32>,
}

impl ProcVolatile {
    fn from_proc(p: &ProcTick) -> Self {
        Self {
            pid: p.pid,
            cpu_pct: p.cpu_pct,
            mem_rss: p.mem_rss,
            mem_virt: p.mem_virt,
            threads: p.threads,
            state: p.state,
            io_read_rate: p.io_read_rate,
            io_write_rate: p.io_write_rate,
            net_rx_rate: p.net_rx_rate,
            net_tx_rate: p.net_tx_rate,
            gpu_pct: p.gpu_pct,
            gpu_mem_bytes: p.gpu_mem_bytes,
            mem_footprint: p.mem_footprint,
            mem_pss: p.mem_pss,
            mem_private: p.mem_private,
            mem_shared: p.mem_shared,
            mem_swap: p.mem_swap,
            mem_peak: p.mem_peak,
            power_w: p.power_w,
        }
    }

    /// Rebuild a full `ProcTick` by pairing this tick's volatile data
    /// with the identity carried (or previously recorded) for its pid.
    fn into_proc(self, identity: &ProcIdentity) -> ProcTick {
        ProcTick {
            pid: self.pid,
            ppid: identity.ppid,
            user: identity.user.clone(),
            name: identity.name.clone(),
            cmd: identity.cmd.clone(),
            cpu_pct: self.cpu_pct,
            mem_rss: self.mem_rss,
            mem_virt: self.mem_virt,
            threads: self.threads,
            state: self.state,
            start_time: identity.start_time,
            io_read_rate: self.io_read_rate,
            io_write_rate: self.io_write_rate,
            net_rx_rate: self.net_rx_rate,
            net_tx_rate: self.net_tx_rate,
            gpu_pct: self.gpu_pct,
            gpu_mem_bytes: self.gpu_mem_bytes,
            mem_footprint: self.mem_footprint,
            mem_pss: self.mem_pss,
            mem_private: self.mem_private,
            mem_shared: self.mem_shared,
            mem_swap: self.mem_swap,
            mem_peak: self.mem_peak,
            power_w: self.power_w,
        }
    }
}

/// One tick, as written to disk. Everything outside the process table
/// and services list is small and genuinely changes most ticks, so
/// it's kept as a direct copy of the matching `Snapshot` fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct EncodedTick {
    t: SystemTime,
    host: HostInfo,
    cpu: CpuTick,
    mem: MemTick,
    disks: Vec<DiskUsageTick>,
    disk_io: DiskIoTick,
    net: Vec<InterfaceTick>,
    gpus: Vec<GpuTick>,
    power: PowerTick,
    net_rates_estimated: bool,
    pressure: Option<PressureTick>,
    /// `None` when identical to the previous tick's services list.
    services: Option<Vec<ServiceTick>>,
    /// Identities not yet recorded (or whose identity changed — pid
    /// reuse) as of this tick.
    new_procs: Vec<ProcIdentity>,
    /// Every live pid's changing data this tick, in `Snapshot::procs`'
    /// original order.
    proc_volatile: Vec<ProcVolatile>,
}

/// Shared identity/services bookkeeping, mirrored on the encode and
/// decode side so each stays in sync with what the other actually
/// wrote/read rather than assuming a periodic reset.
#[derive(Default)]
struct IdentityTracker {
    known: HashMap<u32, ProcIdentity>,
    last_services: Option<Vec<ServiceTick>>,
}

impl IdentityTracker {
    fn encode(&mut self, snap: &Snapshot) -> EncodedTick {
        let mut new_procs = Vec::new();
        let mut proc_volatile = Vec::with_capacity(snap.procs.len());
        for p in &snap.procs {
            let identity = ProcIdentity::from_proc(p);
            let changed = self.known.get(&p.pid) != Some(&identity);
            if changed {
                self.known.insert(p.pid, identity.clone());
                new_procs.push(identity);
            }
            proc_volatile.push(ProcVolatile::from_proc(p));
        }

        let services = if self.last_services.as_ref() == Some(&snap.services) {
            None
        } else {
            self.last_services = Some(snap.services.clone());
            Some(snap.services.clone())
        };

        EncodedTick {
            t: snap.t,
            host: snap.host.clone(),
            cpu: snap.cpu.clone(),
            mem: snap.mem.clone(),
            disks: snap.disks.clone(),
            disk_io: snap.disk_io.clone(),
            net: snap.net.clone(),
            gpus: snap.gpus.clone(),
            power: snap.power.clone(),
            net_rates_estimated: snap.net_rates_estimated,
            pressure: snap.pressure,
            services,
            new_procs,
            proc_volatile,
        }
    }

    /// Rebuild a `Snapshot` from a decoded tick, or `None` if it
    /// references a pid whose identity was never recorded (or a
    /// services delta with no prior list to fall back on) — a
    /// malformed record this tracker didn't itself produce, treated
    /// like any other corruption: stop here, keep what decoded so far.
    fn decode(&mut self, tick: EncodedTick) -> Option<Snapshot> {
        for identity in tick.new_procs {
            self.known.insert(identity.pid, identity);
        }
        let services = match tick.services {
            Some(v) => {
                self.last_services = Some(v.clone());
                v
            }
            None => self.last_services.clone()?,
        };
        let mut procs = Vec::with_capacity(tick.proc_volatile.len());
        for volatile in tick.proc_volatile {
            let identity = self.known.get(&volatile.pid)?;
            procs.push(volatile.into_proc(identity));
        }
        Some(Snapshot {
            t: tick.t,
            host: tick.host,
            cpu: tick.cpu,
            mem: tick.mem,
            disks: tick.disks,
            disk_io: tick.disk_io,
            net: tick.net,
            procs,
            gpus: tick.gpus,
            power: tick.power,
            services,
            net_rates_estimated: tick.net_rates_estimated,
            pressure: tick.pressure,
        })
    }
}

fn write_header(w: &mut impl Write) -> Result<()> {
    w.write_all(MAGIC)?;
    w.write_all(&FORMAT_VERSION.to_le_bytes())?;
    Ok(())
}

fn read_header(r: &mut impl Read) -> Result<()> {
    let mut header = [0u8; 6];
    r.read_exact(&mut header)?;
    if &header[0..4] != MAGIC {
        return Err(anyhow!(
            "not a syswatch recording (magic mismatch: {:?})",
            &header[0..4]
        ));
    }
    let version = u16::from_le_bytes([header[4], header[5]]);
    if version > FORMAT_VERSION {
        return Err(anyhow!(
            "recording format v{} is newer than this binary (max v{})",
            version,
            FORMAT_VERSION
        ));
    }
    if version < FORMAT_VERSION {
        return Err(anyhow!(
            "recording format v{} predates this binary (needs v{}); \
             re-record with the current syswatch",
            version,
            FORMAT_VERSION
        ));
    }
    Ok(())
}

// ── Writer ───────────────────────────────────────────────────────────────

/// Open file recorder. Buffers up to [`BLOCK_TICKS`] ticks, then
/// zstd-compresses and flushes them as one block; [`Drop`] flushes
/// whatever's left so a partial block isn't lost when the user quits.
pub struct Recorder {
    path: PathBuf,
    writer: BufWriter<File>,
    tracker: IdentityTracker,
    /// Length-prefixed postcard records for the block being built.
    pending: Vec<u8>,
    pending_ticks: usize,
    /// Number of snapshots successfully appended (for the footer flash
    /// and "stop" status line).
    pub count: u64,
}

impl Recorder {
    pub fn create(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)?;
        let mut writer = BufWriter::new(file);
        write_header(&mut writer)?;
        Ok(Self {
            path,
            writer,
            tracker: IdentityTracker::default(),
            pending: Vec::new(),
            pending_ticks: 0,
            count: 0,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Encode + buffer one snapshot, flushing a compressed block once
    /// [`BLOCK_TICKS`] have accumulated. Errors here are non-fatal —
    /// caller decides whether to surface them or just stop recording.
    pub fn push(&mut self, snap: &Snapshot) -> Result<()> {
        let tick = self.tracker.encode(snap);
        let bytes =
            postcard::to_allocvec(&tick).map_err(|e| anyhow!("postcard encode failed: {}", e))?;
        let len = u32::try_from(bytes.len())
            .map_err(|_| anyhow!("encoded tick too large for u32 length prefix"))?;
        self.pending.extend_from_slice(&len.to_le_bytes());
        self.pending.extend_from_slice(&bytes);
        self.pending_ticks += 1;
        self.count += 1;

        if self.pending_ticks >= BLOCK_TICKS {
            self.flush_block()?;
        }
        Ok(())
    }

    fn flush_block(&mut self) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let compressed = zstd::stream::encode_all(self.pending.as_slice(), ZSTD_LEVEL)
            .context("zstd block compression failed")?;
        let len = u32::try_from(compressed.len())
            .map_err(|_| anyhow!("compressed block too large for u32 length prefix"))?;
        self.writer.write_all(&len.to_le_bytes())?;
        self.writer.write_all(&compressed)?;
        self.pending.clear();
        self.pending_ticks = 0;
        Ok(())
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        // Best-effort — there's no good place to surface an error from
        // Drop, but losing the buffered tail (up to BLOCK_TICKS ticks)
        // at quit time is worse than logging via stderr (which would
        // corrupt the TUI).
        let _ = self.flush_block();
        let _ = self.writer.flush();
    }
}

// ── Reader ───────────────────────────────────────────────────────────────

/// Streaming reader over a .swr file. Decompresses and decodes one
/// block at a time rather than materializing the whole recording up
/// front — a caller that wants to process ticks as they arrive can use
/// this directly. [`read`] below collects it into a `Vec` for the one
/// caller (`--replay`) that needs random access for scrubbing today;
/// switching that to consume ticks incrementally is a natural
/// follow-up, not required by this format change.
///
/// Tolerant of a truncated or corrupt tail, same as v1/v2: iteration
/// just stops (`next()` returns `None`) at the first bad block or
/// record rather than erroring, so a recording cut off by a crash
/// still replays everything captured before that point.
pub struct RecordingReader {
    reader: BufReader<File>,
    tracker: IdentityTracker,
    pending: VecDeque<Snapshot>,
    done: bool,
}

impl RecordingReader {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        read_header(&mut reader)?;
        Ok(Self {
            reader,
            tracker: IdentityTracker::default(),
            pending: VecDeque::new(),
            done: false,
        })
    }

    /// Decompress and decode the next block, filling `pending`. Leaves
    /// `pending` empty and sets `done` on EOF, a truncated block, or a
    /// record that fails to decode — the tolerant-tail contract.
    fn fill(&mut self) {
        let mut block_len_buf = [0u8; 4];
        match self.reader.read_exact(&mut block_len_buf) {
            Ok(()) => {}
            Err(_) => {
                self.done = true;
                return;
            }
        }
        let block_len = u32::from_le_bytes(block_len_buf) as usize;
        let mut compressed = vec![0u8; block_len];
        if self.reader.read_exact(&mut compressed).is_err() {
            self.done = true;
            return;
        }
        let decompressed = match zstd::stream::decode_all(compressed.as_slice()) {
            Ok(d) => d,
            Err(_) => {
                self.done = true;
                return;
            }
        };

        let mut pos = 0usize;
        while pos + 4 <= decompressed.len() {
            let len = u32::from_le_bytes(decompressed[pos..pos + 4].try_into().unwrap()) as usize;
            pos += 4;
            if pos + len > decompressed.len() {
                self.done = true;
                return;
            }
            let record = &decompressed[pos..pos + len];
            pos += len;
            match postcard::from_bytes::<EncodedTick>(record) {
                Ok(tick) => match self.tracker.decode(tick) {
                    Some(mut s) => {
                        // Scrub on read, not just on capture: a recording
                        // can be handed over by someone else, and may
                        // predate the scrubbing in `Collector::sample`
                        // (issue #21).
                        crate::collect::sanitize::scrub_snapshot(&mut s);
                        self.pending.push_back(s);
                    }
                    None => {
                        self.done = true;
                        return;
                    }
                },
                Err(_) => {
                    self.done = true;
                    return;
                }
            }
        }
    }
}

impl Iterator for RecordingReader {
    type Item = Snapshot;

    fn next(&mut self) -> Option<Snapshot> {
        if self.pending.is_empty() && !self.done {
            self.fill();
        }
        self.pending.pop_front()
    }
}

/// Read every snapshot in a .swr file. Tolerant of truncated tails
/// (returns what it could parse) so a recording cut off by a crash
/// is still useful.
pub fn read(path: &Path) -> Result<Vec<Snapshot>> {
    Ok(RecordingReader::open(path)?.collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn snap_at(secs: u64) -> Snapshot {
        Snapshot {
            t: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
            ..Default::default()
        }
    }

    fn proc(pid: u32, name: &str, cpu: f32) -> ProcTick {
        ProcTick {
            pid,
            ppid: 1,
            user: "matt".into(),
            name: name.into(),
            cmd: format!("/usr/bin/{name}"),
            cpu_pct: cpu,
            start_time: Some(SystemTime::UNIX_EPOCH),
            ..Default::default()
        }
    }

    fn service(name: &str) -> ServiceTick {
        ServiceTick {
            name: name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn round_trip_three_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.swr");
        {
            let mut rec = Recorder::create(path.clone()).unwrap();
            rec.push(&snap_at(1)).unwrap();
            rec.push(&snap_at(2)).unwrap();
            rec.push(&snap_at(3)).unwrap();
            assert_eq!(rec.count, 3);
        }
        let read_back = read(&path).unwrap();
        assert_eq!(read_back.len(), 3);
        assert_eq!(
            read_back[0].t,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1)
        );
        assert_eq!(
            read_back[2].t,
            SystemTime::UNIX_EPOCH + Duration::from_secs(3)
        );
    }

    #[test]
    fn round_trip_preserves_process_identity_and_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("procs.swr");
        let mut s1 = snap_at(1);
        s1.procs = vec![proc(100, "chrome", 40.0), proc(200, "sshd", 0.1)];
        let mut s2 = snap_at(2);
        // Same pids, changed volatile data, identity unchanged.
        s2.procs = vec![proc(100, "chrome", 55.0), proc(200, "sshd", 0.2)];
        {
            let mut rec = Recorder::create(path.clone()).unwrap();
            rec.push(&s1).unwrap();
            rec.push(&s2).unwrap();
        }
        let back = read(&path).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].procs[0].name, "chrome");
        assert_eq!(back[0].procs[0].cpu_pct, 40.0);
        assert_eq!(back[1].procs[0].name, "chrome");
        assert_eq!(back[1].procs[0].cpu_pct, 55.0);
        assert_eq!(back[1].procs[1].name, "sshd");
        // Order preserved even though identity for both pids was only
        // sent once (on the first tick).
        assert_eq!(back[1].procs[0].pid, 100);
        assert_eq!(back[1].procs[1].pid, 200);
    }

    #[test]
    fn pid_reuse_gets_a_fresh_identity_not_the_stale_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reuse.swr");
        let mut s1 = snap_at(1);
        s1.procs = vec![ProcTick {
            pid: 500,
            name: "old-proc".into(),
            start_time: Some(SystemTime::UNIX_EPOCH),
            ..Default::default()
        }];
        let mut s2 = snap_at(2);
        // Same pid, different process: different name and start_time.
        s2.procs = vec![ProcTick {
            pid: 500,
            name: "new-proc".into(),
            start_time: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(2)),
            ..Default::default()
        }];
        {
            let mut rec = Recorder::create(path.clone()).unwrap();
            rec.push(&s1).unwrap();
            rec.push(&s2).unwrap();
        }
        let back = read(&path).unwrap();
        assert_eq!(back[0].procs[0].name, "old-proc");
        assert_eq!(back[1].procs[0].name, "new-proc");
        assert_eq!(
            back[1].procs[0].start_time,
            Some(SystemTime::UNIX_EPOCH + Duration::from_secs(2))
        );
    }

    #[test]
    fn unchanged_services_are_not_resent_but_still_decode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("services.swr");
        let mut s1 = snap_at(1);
        s1.services = vec![service("sshd"), service("cron")];
        let mut s2 = snap_at(2);
        s2.services = s1.services.clone(); // identical
        let mut s3 = snap_at(3);
        s3.services = vec![service("sshd")]; // changed
        {
            let mut rec = Recorder::create(path.clone()).unwrap();
            rec.push(&s1).unwrap();
            rec.push(&s2).unwrap();
            rec.push(&s3).unwrap();
        }
        let back = read(&path).unwrap();
        assert_eq!(back[0].services.len(), 2);
        assert_eq!(back[1].services.len(), 2);
        assert_eq!(back[1].services, back[0].services);
        assert_eq!(back[2].services.len(), 1);
    }

    #[test]
    fn spans_multiple_blocks() {
        // BLOCK_TICKS is 60 -- push enough ticks to force at least two
        // compressed blocks and confirm the stream reassembles cleanly
        // across the boundary.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("multiblock.swr");
        let n = BLOCK_TICKS * 2 + 5;
        {
            let mut rec = Recorder::create(path.clone()).unwrap();
            for i in 0..n {
                let mut s = snap_at(i as u64);
                s.procs = vec![proc(1000 + (i % 3) as u32, "worker", i as f32 % 100.0)];
                rec.push(&s).unwrap();
            }
        }
        let back = read(&path).unwrap();
        assert_eq!(back.len(), n);
        assert_eq!(back[0].t, SystemTime::UNIX_EPOCH);
        assert_eq!(
            back[n - 1].t,
            SystemTime::UNIX_EPOCH + Duration::from_secs((n - 1) as u64)
        );
    }

    #[test]
    fn streaming_reader_yields_the_same_snapshots_as_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stream.swr");
        {
            let mut rec = Recorder::create(path.clone()).unwrap();
            for i in 0..10u64 {
                rec.push(&snap_at(i)).unwrap();
            }
        }
        let via_vec = read(&path).unwrap();
        let via_iter: Vec<Snapshot> = RecordingReader::open(&path).unwrap().collect();
        assert_eq!(via_vec.len(), via_iter.len());
        for (a, b) in via_vec.iter().zip(via_iter.iter()) {
            assert_eq!(a.t, b.t);
        }
    }

    #[test]
    fn rejects_wrong_magic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not_swr.bin");
        fs::write(&path, b"NOPE\x01\x00").unwrap();
        let err = read(&path).unwrap_err();
        assert!(err.to_string().contains("not a syswatch recording"));
    }

    #[test]
    fn rejects_future_format_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("future.swr");
        let mut bytes = Vec::from(*MAGIC);
        bytes.extend_from_slice(&999u16.to_le_bytes());
        fs::write(&path, &bytes).unwrap();
        let err = read(&path).unwrap_err();
        assert!(err.to_string().contains("newer than this binary"));
    }

    #[test]
    fn rejects_older_format_version() {
        // v2 (and v1) recordings predate this format's shape and can't
        // be decoded by this binary — better a clear error than an
        // empty or garbled replay.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.swr");
        let mut bytes = Vec::from(*MAGIC);
        bytes.extend_from_slice(&2u16.to_le_bytes());
        fs::write(&path, &bytes).unwrap();
        let err = read(&path).unwrap_err();
        assert!(err.to_string().contains("predates this binary"));
    }

    #[test]
    fn truncated_block_length_returns_partial() {
        // One full block, then a truncated 4-byte block-length prefix
        // that claims more bytes than the file contains. read() should
        // return the complete block's snapshots and stop cleanly.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("truncated.swr");
        {
            let mut rec = Recorder::create(path.clone()).unwrap();
            rec.push(&snap_at(10)).unwrap();
            rec.push(&snap_at(20)).unwrap();
            // Force the block to flush now rather than staying buffered
            // (Drop would flush it too, but be explicit).
            rec.flush_block().unwrap();
        }
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&999_999u32.to_le_bytes()).unwrap();
        drop(f);
        let snaps = read(&path).unwrap();
        assert_eq!(snaps.len(), 2);
    }

    #[test]
    fn corrupt_compressed_block_returns_partial() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.swr");
        {
            let mut rec = Recorder::create(path.clone()).unwrap();
            rec.push(&snap_at(1)).unwrap();
            rec.flush_block().unwrap();
        }
        // Append a block-length prefix + garbage that isn't valid zstd.
        let garbage = vec![0xFFu8; 16];
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&(garbage.len() as u32).to_le_bytes()).unwrap();
        f.write_all(&garbage).unwrap();
        drop(f);
        let snaps = read(&path).unwrap();
        assert_eq!(snaps.len(), 1);
    }
}
