//! On-demand snapshot dump (`S` key).
//!
//! Writes the current `Snapshot` to a timestamped JSON file under the
//! platform's local-data directory. Pure file IO — no network, no
//! background work — so the user can grab a one-shot diagnostic with
//! a single keystroke and share the resulting file.
//!
//! This is the foundation for the recording story (`R`, deferred to
//! Phase 2): once we have binary session capture, JSON snapshots stay
//! as the human-readable single-tick variant.

use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};

use crate::collect::Snapshot;

/// Directory snapshots get written to. Created on first write.
///
/// - Linux: `~/.local/share/syswatch/snapshots`
/// - macOS: `~/Library/Application Support/syswatch/snapshots`
/// - Windows: `%LOCALAPPDATA%/syswatch/snapshots`
pub fn dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join("syswatch").join("snapshots"))
}

/// Write `snap` as pretty-printed JSON. Returns the absolute path on
/// success so the UI can show it in a status flash.
pub fn write(snap: &Snapshot) -> anyhow::Result<PathBuf> {
    let dir = dir().ok_or_else(|| anyhow::anyhow!("cannot determine local data directory"))?;
    write_in(&dir, snap)
}

/// [`write`] into `dir`. The snapshot lists every process's full command
/// line, so the file is 0600 and `dir` is 0700. The file is always a new
/// one: a second snapshot within the same second, or anything else already
/// at that name, gets a numbered name instead (`snap-…-1.json`).
fn write_in(dir: &Path, snap: &Snapshot) -> anyhow::Result<PathBuf> {
    crate::owner_only::create_dir_all(dir)?;
    let ts: DateTime<Local> = snap.t.into();
    // Filesystem-safe ISO-8601 (no colons — those break Windows paths).
    let stem = ts.format("snap-%Y-%m-%dT%H-%M-%S").to_string();
    let body = serde_json::to_string_pretty(snap)?;
    let (path, mut file) = crate::owner_only::create_new_numbered(dir, &stem, "json")?;
    if let Err(e) = file.write_all(body.as_bytes()) {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err(e.into());
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_resolves_under_local_data() {
        // The directory lookup itself is purely a join on the platform
        // local-data dir; we just verify the suffix shape.
        let Some(d) = dir() else { return };
        let s = d.to_string_lossy();
        assert!(s.contains("syswatch"));
        assert!(s.ends_with("snapshots"));
    }

    fn near_empty_snapshot() -> Snapshot {
        Snapshot {
            t: std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000),
            host: Default::default(),
            cpu: Default::default(),
            mem: Default::default(),
            disks: Vec::new(),
            disk_io: Default::default(),
            net: Vec::new(),
            procs: Vec::new(),
            gpus: Vec::new(),
            power: Default::default(),
            services: Vec::new(),
            net_rates_estimated: true,
            pressure: None,
        }
    }

    #[test]
    fn writes_snapshot_round_trips() {
        // Round-trip a near-empty snapshot through serde_json's parser to
        // confirm the on-disk format is stable enough for downstream
        // consumption.
        let snap = near_empty_snapshot();
        let body = serde_json::to_string_pretty(&snap).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&body).expect("re-parse");
        // Sanity: a few top-level keys we expect.
        assert!(v.get("host").is_some());
        assert!(v.get("cpu").is_some());
        assert!(v.get("procs").is_some());
    }

    #[test]
    fn written_file_parses_back() {
        let root = tempfile::tempdir().unwrap();
        let path = write_in(&root.path().join("snapshots"), &near_empty_snapshot()).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(v.get("procs").is_some());
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("syswatch").join("snapshots");
        let path = write_in(&dir, &near_empty_snapshot()).unwrap();
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&dir), 0o700);

        // A directory an older syswatch left at 0755 is narrowed on the next write.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        write_in(&dir, &near_empty_snapshot()).unwrap();
        assert_eq!(mode(&dir), 0o700);
    }

    #[test]
    fn a_second_snapshot_in_the_same_second_gets_a_new_file() {
        let root = tempfile::tempdir().unwrap();
        let first = write_in(root.path(), &near_empty_snapshot()).unwrap();
        let second = write_in(root.path(), &near_empty_snapshot()).unwrap();
        assert_ne!(first, second);
        let stem = first.file_stem().unwrap().to_string_lossy().into_owned();
        assert_eq!(second, root.path().join(format!("{stem}-1.json")));
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_entry_at_the_snapshot_name_is_left_alone() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("snapshots");
        std::fs::create_dir(&dir).unwrap();
        let other = root.path().join("other.txt");
        std::fs::write(&other, "original").unwrap();
        // Find the name the snapshot would take, then put a link there.
        let name = write_in(&dir, &near_empty_snapshot()).unwrap();
        std::fs::remove_file(&name).unwrap();
        std::os::unix::fs::symlink(&other, &name).unwrap();

        let written = write_in(&dir, &near_empty_snapshot()).unwrap();
        assert_ne!(written, name);
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "original");
        assert!(std::fs::symlink_metadata(&name)
            .unwrap()
            .file_type()
            .is_symlink());
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&written).unwrap()).unwrap();
        assert!(v.get("procs").is_some());
    }
}
