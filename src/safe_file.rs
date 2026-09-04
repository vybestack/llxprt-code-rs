//! Fail-fast primitives for bounded external configuration inputs and the
//! crash-safe `context/` artifact publications (issue #102).
//!
//! Publications follow the one atomic step per file rule: payload to a private
//! temporary file in the target directory, fsync on the payload file, a
//! same-directory rename over the final name, then fsync on the containing
//! directory. A crash anywhere leaves either the old or the new artifact.

use std::path::Path;

/// Open an existing regular file without following its final path component.
///
/// `O_NONBLOCK` prevents a FIFO or device from hanging before its type is checked. The
/// descriptor itself is inspected after opening, so a pre-open metadata race cannot change the
/// accepted object type.
pub(crate) fn open_regular_nofollow(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "input is not a regular file",
        ));
    }
    Ok(file)
}

/// Writes `bytes` to `name` inside an already-open private directory through a
/// temp file in the same directory, an fsync on the payload, a rename, and an
/// fsync on the directory: the publication is all-or-nothing per file and the
/// rename itself survives a crash. Temp files are dot-prefixed so artifact
/// enumerations can ignore half-published state.
pub(crate) fn publish_artifact(
    dir: &openat::Dir,
    name: &str,
    bytes: &[u8],
    open: impl Fn(&openat::Dir, &str, libc::c_int, libc::mode_t) -> std::io::Result<std::fs::File>,
) -> Result<(), std::io::Error> {
    use std::io::Write as _;

    let temp = format!(".{name}.tmp");
    let mut file = open(
        dir,
        &temp,
        libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
        0o600,
    )?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    dir.local_rename(&temp, name)?;
    let handle = dir.open_file(".").map_err(std::io::Error::other)?;
    handle.sync_all()
}

/// Why an artifact read failed, classified from the raw errno at the call site
/// so "absent" is never confused with "unreadable" (issue 102).
///
/// `NotFound` is the ONLY absence verdict: every other failure (a symlinked
/// name, `EACCES`, `ENOTDIR`, `EMFILE`, a non-regular or over-bound artifact,
/// a failed read) is a distinct typed failure the caller must surface instead
/// of silently minting fresh state over it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArtifactError {
    /// The name does not exist under the directory: a fresh store is legal.
    NotFound { name: String },
    /// The name exists but could not be opened: never treated as absence.
    Unopenable {
        name: String,
        kind: std::io::ErrorKind,
    },
    /// The name opened but is not a regular file.
    NotRegular { name: String },
    /// The read itself failed.
    Unreadable {
        name: String,
        kind: std::io::ErrorKind,
    },
    /// The artifact exceeded its read bound.
    OverBound { name: String, max: usize },
    /// The artifact name is not a valid C string.
    BadName { name: String },
}

impl std::fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArtifactError::NotFound { name } => write!(f, "artifact {name} not found"),
            ArtifactError::Unopenable { name, kind } => {
                write!(f, "open artifact {name} failed: {kind}")
            }
            ArtifactError::NotRegular { name } => {
                write!(f, "artifact {name} is not a regular file")
            }
            ArtifactError::Unreadable { name, kind } => {
                write!(f, "read artifact {name} failed: {kind}")
            }
            ArtifactError::OverBound { name, max } => {
                write!(f, "artifact {name} exceeds the read bound {max}")
            }
            ArtifactError::BadName { name } => write!(f, "artifact name {name} is invalid"),
        }
    }
}

impl std::error::Error for ArtifactError {}

/// Reads one regular file through an already-open directory descriptor with
/// `O_NOFOLLOW`, enforcing a read bound so a corrupted or hostile artifact
/// cannot force an unbounded allocation. Failures are typed from the raw errno,
/// so only `ArtifactError::NotFound` means the artifact is absent (issue 102).
pub(crate) fn read_artifact(
    dir: &openat::Dir,
    name: &str,
    max: usize,
) -> Result<Vec<u8>, ArtifactError> {
    use std::io::Read as _;
    use std::os::fd::{AsRawFd as _, FromRawFd as _};

    let flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
    let cstring = std::ffi::CString::new(name).map_err(|_| ArtifactError::BadName {
        name: name.to_string(),
    })?;
    let fd = unsafe { libc::openat(dir.as_raw_fd(), cstring.as_ptr(), flags, 0o600) };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        // Classify by the raw errno at the open call site: only ENOENT
        // is absence (a fresh store or a first key); every other
        // failure - a symlinked name with O_NOFOLLOW, EACCES, ENOTDIR,
        // EMFILE, ... - is an unreadable artifact the caller must
        // surface, never a silent reset (issue 102).
        if error.kind() == std::io::ErrorKind::NotFound {
            return Err(ArtifactError::NotFound {
                name: name.to_string(),
            });
        }
        return Err(ArtifactError::Unopenable {
            name: name.to_string(),
            kind: error.kind(),
        });
    }
    // SAFETY: the descriptor is owned from here on and `File` closes it on drop.
    let file = unsafe { std::fs::File::from(std::os::fd::OwnedFd::from_raw_fd(fd)) };
    if !file.metadata().map(|meta| meta.is_file()).unwrap_or(false) {
        return Err(ArtifactError::NotRegular {
            name: name.to_string(),
        });
    }
    let mut bytes = Vec::new();
    file.take((max as u64) + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| ArtifactError::Unreadable {
            name: name.to_string(),
            kind: error.kind(),
        })?;
    if bytes.len() > max {
        return Err(ArtifactError::OverBound {
            name: name.to_string(),
            max,
        });
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regular_file_opens_and_symlink_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let regular = temp.path().join("regular");
        std::fs::write(&regular, "value").unwrap();
        assert!(open_regular_nofollow(&regular).is_ok());

        std::os::unix::fs::symlink(&regular, temp.path().join("link")).unwrap();
        assert!(open_regular_nofollow(&temp.path().join("link")).is_err());
    }

    #[test]
    fn fifo_is_rejected_without_waiting_for_a_writer() {
        use std::os::unix::ffi::OsStrExt;

        let temp = tempfile::tempdir().unwrap();
        let fifo = temp.path().join("fifo");
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(open_regular_nofollow(&fifo).is_err());
    }

    #[test]
    fn publish_replaces_and_reads_back_through_the_dir() {
        let temp = tempfile::tempdir().unwrap();
        let dir = openat::Dir::open(temp.path()).unwrap();
        let open = |dir: &openat::Dir, name: &str, flags: libc::c_int, mode: libc::mode_t| {
            crate::session::open_regular_at(dir, name, flags, mode)
        };
        publish_artifact(&dir, "artifact", b"first", open).unwrap();
        assert_eq!(read_artifact(&dir, "artifact", 1024).unwrap(), b"first");
        publish_artifact(&dir, "artifact", b"second", open).unwrap();
        assert_eq!(read_artifact(&dir, "artifact", 1024).unwrap(), b"second");
        assert_eq!(
            dir.list_dir(".").unwrap().count(),
            1,
            "no temp file survives"
        );
    }

    #[test]
    fn read_artifact_enforces_the_bound() {
        let temp = tempfile::tempdir().unwrap();
        let dir = openat::Dir::open(temp.path()).unwrap();
        let open = |dir: &openat::Dir, name: &str, flags: libc::c_int, mode: libc::mode_t| {
            crate::session::open_regular_at(dir, name, flags, mode)
        };
        publish_artifact(&dir, "big", &[0u8; 64], open).unwrap();
        let error = read_artifact(&dir, "big", 8).unwrap_err();
        assert!(
            matches!(error, ArtifactError::OverBound { max, .. } if max == 8),
            "the read bound is a typed failure: {error}"
        );
    }

    #[test]
    fn read_artifact_rejects_symlinked_names() {
        let temp = tempfile::tempdir().unwrap();
        let dir = openat::Dir::open(temp.path()).unwrap();
        std::fs::write(temp.path().join("target"), b"secret").unwrap();
        std::os::unix::fs::symlink("target", temp.path().join("link")).unwrap();
        assert!(read_artifact(&dir, "link", 1024).is_err());
    }
}
