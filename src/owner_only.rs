//! Files and directories only their owner can open, for what syswatch saves:
//! recordings, snapshots and the config.
//!
//! Recordings and snapshots hold every visible process's full command line,
//! and a command line can carry a password or a token. Created under the
//! usual umask they came out 0644 in 0755 directories. On Unix these helpers
//! set the mode as the file or directory is created, so there is no moment
//! when someone else can open it. Elsewhere they are the plain `std::fs`
//! calls.

use std::fs::{DirBuilder, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
pub(crate) const FILE_MODE: u32 = 0o600;
#[cfg(unix)]
const DIR_MODE: u32 = 0o700;

/// Write options whose new files are 0600. The caller adds `create_new` or
/// `truncate`. The mode applies only to a file the open creates.
pub fn file_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(FILE_MODE);
    }
    options
}

/// Create a new 0600 file in `dir` named `stem.ext`, or `stem-1.ext`,
/// `stem-2.ext` and so on when that name is taken. Each attempt is a
/// `create_new` open, which fails on any existing entry, a symlink
/// included, so nothing already at one of those names is written to.
pub fn create_new_numbered(dir: &Path, stem: &str, ext: &str) -> io::Result<(PathBuf, File)> {
    const MAX_SUFFIX: u32 = 1000;
    for n in 0..=MAX_SUFFIX {
        let name = if n == 0 {
            format!("{stem}.{ext}")
        } else {
            format!("{stem}-{n}.{ext}")
        };
        let path = dir.join(name);
        match file_options().create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("{stem}.{ext} and {stem}-1.{ext} to {stem}-{MAX_SUFFIX}.{ext} all exist"),
    ))
}

/// Create `dir` and any missing parents, 0700. A `dir` that already existed,
/// say 0755 from an older syswatch, is narrowed to 0700 when this user owns
/// it, which also hides the files an older syswatch left in it. A symlink,
/// or a directory someone else owns, is left as it is: that is where the
/// user pointed it, and `fs::create_dir_all` accepted it too.
pub fn create_dir_all(dir: &Path) -> io::Result<()> {
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(DIR_MODE);
    }
    builder.create(dir)?;
    #[cfg(unix)]
    narrow_dir(dir, effective_uid());
    Ok(())
}

#[cfg(unix)]
pub(crate) fn effective_uid() -> u32 {
    // SAFETY: geteuid takes no arguments, cannot fail and touches no memory.
    unsafe { libc::geteuid() }
}

/// Narrow `dir` to 0700 when `uid` owns it. A directory with another owner
/// keeps the mode its owner set.
#[cfg(unix)]
fn narrow_dir(dir: &Path, uid: u32) {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    let Ok(handle) = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(dir)
    else {
        return;
    };
    if handle.metadata().is_ok_and(|meta| meta.uid() == uid) {
        let _ = handle.set_permissions(std::fs::Permissions::from_mode(DIR_MODE));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    fn mode(path: &Path) -> u32 {
        std::fs::symlink_metadata(path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    }

    #[test]
    fn new_directories_and_files_are_owner_only() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("syswatch").join("snapshots");
        create_dir_all(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);
        // The parents it created are 0700 less the umask, which only removes bits.
        assert_eq!(mode(&root.path().join("syswatch")) & 0o077, 0);

        let (path, _) = create_new_numbered(&dir, "snap", "json").unwrap();
        assert_eq!(path, dir.join("snap.json"));
        assert_eq!(mode(&path), 0o600);
    }

    #[test]
    fn an_existing_directory_is_narrowed() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("sessions");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        create_dir_all(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);
    }

    #[test]
    fn a_taken_name_gets_the_next_number() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        let other = dir.join("other.txt");
        std::fs::write(&other, "original").unwrap();
        std::os::unix::fs::symlink(&other, dir.join("snap.json")).unwrap();
        std::os::unix::fs::symlink(dir.join("missing"), dir.join("snap-1.json")).unwrap();
        std::fs::write(dir.join("snap-2.json"), "older").unwrap();

        let (path, mut file) = create_new_numbered(dir, "snap", "json").unwrap();
        file.write_all(b"new").unwrap();
        assert_eq!(path, dir.join("snap-3.json"));
        assert_eq!(mode(&path), 0o600);
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "original");
        assert!(
            !dir.join("missing").exists(),
            "a dangling link is not created through"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("snap-2.json")).unwrap(),
            "older"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
    }

    #[test]
    fn a_directory_someone_else_owns_is_not_narrowed() {
        use std::os::unix::fs::MetadataExt;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("shared");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Only root can chmod another user's directory, so the owner check is
        // exercised with a uid that does not own this one.
        let owner = std::fs::metadata(&dir).unwrap().uid();
        narrow_dir(&dir, owner.wrapping_add(1));
        assert_eq!(mode(&dir), 0o755);
        narrow_dir(&dir, owner);
        assert_eq!(mode(&dir), 0o700);
    }

    #[test]
    fn a_symlinked_directory_is_used_but_not_changed() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("elsewhere");
        std::fs::create_dir(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        let link = root.path().join("ring");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        create_dir_all(&link).unwrap();
        assert_eq!(mode(&target), 0o755);
    }
}
