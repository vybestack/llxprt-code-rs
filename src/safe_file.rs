//! Fail-fast opening for bounded external configuration inputs.

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
    if !file.metadata()?.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "input is not a regular file",
        ));
    }
    Ok(file)
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
}
