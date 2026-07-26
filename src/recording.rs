//! Session recording (.swr files).
//!
//! When the user presses `R`, syswatch starts appending each tick's
//! `Snapshot` to a binary file under the local data dir. Pressing `R`
//! again (or quitting) flushes and closes. The file is then replayable
//! via `syswatch --replay path/to/session.swr`.
//!
//! Format: a 6-byte header (`b"SWR\0" + u16 version`) followed by a
//! stream of length-prefixed postcard records — `u32 length` (LE) +
//! `length` bytes of postcard-encoded `Snapshot`. Length-prefixed
//! framing keeps the format trivial to truncate-recover (a partial
//! tail record at process crash is dropped on replay) without
//! depending on COBS/SLIP machinery.

use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use chrono::{DateTime, Local};

use crate::collect::Snapshot;

/// Magic bytes at the start of every .swr file. ASCII so `file(1)`
/// can identify them later if we add a magic database entry.
pub const MAGIC: &[u8; 4] = b"SWR\0";
/// Bumped on incompatible format changes. postcard isn't
/// self-describing, so any change to `Snapshot`'s shape (v2: per-proc
/// memory detail fields on `ProcTick` + `net_rates_estimated`) is
/// incompatible both ways —
/// replay refuses any version other than its own with a clear error
/// rather than silently returning zero snapshots.
pub const FORMAT_VERSION: u16 = 2;

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

/// Open file recorder. Each call to [`Recorder::push`] writes one
/// length-prefixed snapshot to the buffered writer; flushed on Drop
/// so any buffered tail isn't lost when the user quits.
pub struct Recorder {
    path: PathBuf,
    writer: BufWriter<File>,
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
        // Header: magic + version.
        writer.write_all(MAGIC)?;
        writer.write_all(&FORMAT_VERSION.to_le_bytes())?;
        Ok(Self {
            path,
            writer,
            count: 0,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Encode + append one snapshot. Errors here are non-fatal —
    /// caller decides whether to surface them or just stop recording.
    pub fn push(&mut self, snap: &Snapshot) -> Result<()> {
        let bytes =
            postcard::to_allocvec(snap).map_err(|e| anyhow!("postcard encode failed: {}", e))?;
        let len = u32::try_from(bytes.len())
            .map_err(|_| anyhow!("snapshot too large for u32 length prefix"))?;
        self.writer.write_all(&len.to_le_bytes())?;
        self.writer.write_all(&bytes)?;
        self.count += 1;
        Ok(())
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        // Best-effort flush — there's no good place to surface an
        // error from Drop, but losing the buffer at quit time is
        // worse than logging via stderr (which would corrupt the TUI).
        let _ = self.writer.flush();
    }
}

/// Read every snapshot in a .swr file. Tolerant of truncated tails
/// (returns what it could parse) so a recording cut off by a crash
/// is still useful.
pub fn read(path: &Path) -> Result<Vec<Snapshot>> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);

    let mut header = [0u8; 6];
    reader.read_exact(&mut header)?;
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

    let mut out = Vec::new();
    loop {
        let mut len_buf = [0u8; 4];
        match reader.read_exact(&mut len_buf) {
            Ok(()) => {}
            // EOF on a record boundary is the normal terminator.
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
        let len = u32::from_le_bytes(len_buf) as usize;
        let mut buf = vec![0u8; len];
        if reader.read_exact(&mut buf).is_err() {
            // Truncated tail — drop it silently and return what we have.
            break;
        }
        match postcard::from_bytes::<Snapshot>(&buf) {
            // Scrub on read, not just on capture: a recording can be
            // handed over by someone else, and may predate the scrubbing
            // in `Collector::sample` (issue #21).
            Ok(mut s) => {
                crate::collect::sanitize::scrub_snapshot(&mut s);
                out.push(s);
            }
            // Bad record — same logic. Stop where the corruption starts
            // rather than discarding the whole file.
            Err(_) => break,
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn snap_at(secs: u64) -> Snapshot {
        Snapshot {
            t: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
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
        // Magic + version = 999.
        let mut bytes = Vec::from(*MAGIC);
        bytes.extend_from_slice(&999u16.to_le_bytes());
        fs::write(&path, &bytes).unwrap();
        let err = read(&path).unwrap_err();
        assert!(err.to_string().contains("newer than this binary"));
    }

    #[test]
    fn rejects_older_format_version() {
        // v1 snapshots predate the per-proc memory detail fields and
        // can't be decoded by this binary — better a clear error than
        // an empty replay.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.swr");
        let mut bytes = Vec::from(*MAGIC);
        bytes.extend_from_slice(&1u16.to_le_bytes());
        fs::write(&path, &bytes).unwrap();
        let err = read(&path).unwrap_err();
        assert!(err.to_string().contains("predates this binary"));
    }

    #[test]
    fn truncated_tail_returns_partial() {
        // Two valid records then a truncated 4-byte length prefix that
        // claims more bytes than the file contains. read() should return
        // both valid records and stop cleanly.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("truncated.swr");
        {
            let mut rec = Recorder::create(path.clone()).unwrap();
            rec.push(&snap_at(10)).unwrap();
            rec.push(&snap_at(20)).unwrap();
        }
        // Append a length prefix promising 999 bytes that aren't there.
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&999u32.to_le_bytes()).unwrap();
        drop(f);
        let snaps = read(&path).unwrap();
        assert_eq!(snaps.len(), 2);
    }
}
