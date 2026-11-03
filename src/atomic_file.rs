//! Replace a file so that a failed write leaves the old one whole.
//!
//! `fs::write` truncates first, so a full disk could leave half a config
//! behind. The new contents go to a temporary 0600 file beside the old one,
//! are synced, and are renamed over it. Modelled on netwatch's
//! `atomic_file`, owner-only throughout.

use std::fs::{self, File, Metadata};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Replace `path` with `bytes`, leaving it 0600. A symlink stays, and its
/// target is what changes, so a config linked from a dotfiles repository
/// keeps working. A link that belongs to another account is not followed.
/// A file this user may not write stays refused, though the
/// directory would allow a rename over it.
///
/// Where the file cannot be replaced at all, this writes in place, as
/// `fs::write` did: a directory this user cannot create in, or a file
/// bind-mounted on its own, which a rename cannot touch.
pub fn replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let target = symlink_target(path)?;
    let existing = match fs::symlink_metadata(&target) {
        Ok(meta) => Some(meta),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    if let Some(meta) = &existing {
        if !meta.is_file() {
            return Err(io::Error::other(format!(
                "{} is not a regular file",
                target.display()
            )));
        }
        fs::OpenOptions::new().write(true).open(&target)?;
    }
    match write_beside(&target, bytes, existing.as_ref()) {
        Err(e)
            if existing.is_some()
                && matches!(
                    e.kind(),
                    io::ErrorKind::PermissionDenied
                        | io::ErrorKind::ResourceBusy
                        | io::ErrorKind::CrossesDevices
                ) =>
        {
            write_in_place(&target, bytes)
        }
        result => result,
    }
}

/// Where `path` ends up once every symlink is followed, whether or not that
/// file exists yet. The limit is the kernel's own, for a loop of links.
fn symlink_target(path: &Path) -> io::Result<PathBuf> {
    let mut current = path.to_path_buf();
    for _ in 0..40 {
        let meta = match fs::symlink_metadata(&current) {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(current),
            Err(e) => return Err(e),
        };
        if !meta.file_type().is_symlink() {
            return Ok(current);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if !link_owner_trusted(meta.uid(), crate::owner_only::effective_uid()) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "{} is a link that belongs to another account",
                        current.display()
                    ),
                ));
            }
        }
        let link = fs::read_link(&current)?;
        current = match current.parent() {
            Some(parent) => parent.join(link),
            None => link,
        };
    }
    Err(io::Error::other(format!(
        "too many levels of symbolic links at {}",
        path.display()
    )))
}

#[cfg(unix)]
fn link_owner_trusted(link_uid: u32, euid: u32) -> bool {
    link_uid == euid || link_uid == 0
}

fn write_beside(target: &Path, bytes: &[u8], existing: Option<&Metadata>) -> io::Result<()> {
    let dir = match target.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    let stem = format!(".{name}.{}", std::process::id());
    let (temp, mut file) = crate::owner_only::create_new_numbered(dir, &stem, "tmp")?;
    let written = fill(&mut file, bytes, existing);
    // Closed before the rename, which Windows refuses on an open file.
    drop(file);
    let replaced = written.and_then(|()| fs::rename(&temp, target));
    if replaced.is_err() {
        let _ = fs::remove_file(&temp);
    }
    replaced?;
    #[cfg(unix)]
    if let Ok(dir) = File::open(dir) {
        // Best effort. The rename has happened, and a crash now is all this covers.
        let _ = dir.sync_all();
    }
    Ok(())
}

fn fill(file: &mut File, bytes: &[u8], existing: Option<&Metadata>) -> io::Result<()> {
    file.write_all(bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if let Some(meta) = existing {
            // Best effort: keep the replaced file's owner and group, as an
            // in-place write would. Where that isn't allowed, the saver owns it.
            let _ = std::os::unix::fs::fchown(&*file, Some(meta.uid()), Some(meta.gid()));
        }
        // The umask may have taken bits off the 0600 the file was created with.
        file.set_permissions(fs::Permissions::from_mode(crate::owner_only::FILE_MODE))?;
    }
    #[cfg(not(unix))]
    let _ = existing;
    file.sync_all()
}

fn write_in_place(target: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = crate::owner_only::file_options();
    options.truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(target)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Best effort, since a file this user can write but not chmod still
        // took a plain write before.
        let _ = file.set_permissions(fs::Permissions::from_mode(crate::owner_only::FILE_MODE));
    }
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn it_replaces_or_creates_and_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        replace(&path, b"one").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"one");
        replace(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert_eq!(names(dir.path()), ["config.toml"]);
    }

    /// A file size limit is a real write failure: the write returns EFBIG, as a
    /// full disk returns ENOSPC. Linux only, for `ulimit -f` and the signal's name.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_failed_write_keeps_the_old_file_and_leaves_no_temporary_file() {
        use std::process::Command;
        const CHILD: &str = "SYSWATCH_ATOMIC_TEST_FILE";
        // Run again under a file size limit, standing in for a full disk.
        if let Some(path) = std::env::var_os(CHILD) {
            let err = replace(Path::new(&path), &vec![b'x'; 64 * 1024]).unwrap_err();
            assert_eq!(err.raw_os_error(), Some(libc::EFBIG), "{err}");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, b"the old contents").unwrap();
        let this_test =
            "atomic_file::tests::a_failed_write_keeps_the_old_file_and_leaves_no_temporary_file";
        let output = Command::new("sh")
            .args(["-c", "trap '' XFSZ; ulimit -f 1; exec \"$0\" \"$@\""])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", this_test])
            .env(CHILD, &path)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "{output:?}"
        );
        assert_eq!(fs::read(&path).unwrap(), b"the old contents");
        assert_eq!(names(dir.path()), ["config.toml"]);
    }

    #[cfg(unix)]
    mod unix {
        use super::*;
        use std::os::unix::fs::{symlink, PermissionsExt};

        fn mode(path: &Path) -> u32 {
            fs::metadata(path).unwrap().permissions().mode() & 0o777
        }

        fn is_root() -> bool {
            crate::owner_only::effective_uid() == 0
        }

        #[test]
        fn the_result_is_owner_only() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.toml");
            fs::write(&path, b"old").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            replace(&path, b"new").unwrap();
            assert_eq!(mode(&path), 0o600);
            replace(&dir.path().join("new.toml"), b"x").unwrap();
            assert_eq!(mode(&dir.path().join("new.toml")), 0o600);
        }

        /// The link stays a link, relative or through another link, and what it
        /// points at changes.
        #[test]
        fn it_follows_this_users_symlink_to_its_target() {
            let dir = tempfile::tempdir().unwrap();
            let dir = dir.path();
            fs::create_dir(dir.join("dotfiles")).unwrap();
            let target = dir.join("dotfiles/config.toml");
            fs::write(&target, b"old").unwrap();
            symlink("dotfiles/config.toml", dir.join("first")).unwrap();
            symlink(dir.join("first"), dir.join("second")).unwrap();

            replace(&dir.join("second"), b"new").unwrap();
            assert_eq!(fs::read(&target).unwrap(), b"new");
            for link in ["first", "second"] {
                assert!(fs::symlink_metadata(dir.join(link))
                    .unwrap()
                    .file_type()
                    .is_symlink());
            }
            assert_eq!(names(&dir.join("dotfiles")), ["config.toml"]);

            // A link to a file that is not there yet creates it, as a write did.
            symlink("dotfiles/later", dir.join("dangling")).unwrap();
            replace(&dir.join("dangling"), b"x").unwrap();
            assert_eq!(fs::read(dir.join("dotfiles/later")).unwrap(), b"x");
        }

        /// A test can't create a link with another owner, so the owner rule
        /// is checked directly.
        #[test]
        fn the_link_owner_rule() {
            assert!(link_owner_trusted(1000, 1000));
            assert!(link_owner_trusted(0, 1000));
            assert!(!link_owner_trusted(1001, 1000));
            assert!(!link_owner_trusted(1000, 0));
            assert!(link_owner_trusted(0, 0));
        }

        #[test]
        fn a_device_or_directory_is_refused() {
            let dir = tempfile::tempdir().unwrap();
            symlink("/dev/null", dir.path().join("null")).unwrap();
            assert!(replace(&dir.path().join("null"), b"x").is_err());
            fs::create_dir(dir.path().join("sub")).unwrap();
            assert!(replace(&dir.path().join("sub"), b"x").is_err());
        }

        /// `chmod a-w` on a config means something, and a rename would ignore it.
        #[test]
        fn a_read_only_file_stays_refused() {
            if is_root() {
                return;
            }
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.toml");
            fs::write(&path, b"old").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o440)).unwrap();
            let err = replace(&path, b"new").unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
            assert_eq!(fs::read(&path).unwrap(), b"old");
        }

        /// A writable file in a directory this user cannot create in worked with a
        /// plain write, so it still does.
        #[test]
        fn a_directory_it_cannot_create_in_is_written_in_place() {
            if is_root() {
                return;
            }
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.toml");
            fs::write(&path, b"old").unwrap();
            fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o500)).unwrap();
            let result = replace(&path, b"new");
            fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
            result.unwrap();
            assert_eq!(fs::read(&path).unwrap(), b"new");
        }
    }
}
