use serde::Serialize;
use std::io::Write as _;
use std::os::fd::{AsRawFd as _, FromRawFd as _};
use std::path::{Path, PathBuf};

pub(super) struct Sink {
    file: std::fs::File,
    parent: openat::Dir,
    path: PathBuf,
    scratch: Vec<u8>,
}

impl Sink {
    pub(super) fn create(path: &Path) -> std::io::Result<Self> {
        let leaf = path.file_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "profile path needs a file name",
            )
        })?;
        if leaf == "." || leaf == ".." {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "profile path needs a regular-file leaf",
            ));
        }
        let parent_path = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let parent = crate::tools::open_root(parent_path).map_err(std::io::Error::other)?;
        let name =
            std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(leaf)).map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL in profile leaf")
            })?;
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY
                    | libc::O_CREAT
                    | libc::O_EXCL
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK
                    | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        if !file.metadata()?.file_type().is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "profile destination is not a regular file",
            ));
        }
        if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            file,
            parent,
            path: path.to_path_buf(),
            scratch: Vec::with_capacity(2048),
        })
    }

    pub(super) fn write_event<T: Serialize>(&mut self, event: &T) -> std::io::Result<()> {
        self.scratch.clear();
        serde_json::to_writer(&mut self.scratch, event).map_err(std::io::Error::other)?;
        self.scratch.push(b'\n');
        self.file.write_all(&self.scratch)
    }

    pub(super) fn sync_file(&self) -> std::io::Result<()> {
        self.file.sync_all()
    }

    pub(super) fn sync_parent(&self) -> std::io::Result<()> {
        self.parent.open_file(".")?.sync_all()
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}
