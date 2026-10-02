use super::super::credentials::CredentialError;
use fs2::FileExt as _;
use std::ffi::CString;
use std::fs::{File, Metadata, Permissions};
use std::io::{Read as _, Write as _};
use std::os::fd::{AsRawFd as _, FromRawFd as _};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Component, Path};
use std::time::{Duration, Instant};

pub(super) const MAX_BYTES: usize = 65_536;
const TOKEN: &str = "codex.json";
const LOCK: &str = "codex.lock";
const LOCK_WAIT: Duration = Duration::from_secs(120);

pub(super) struct LockedStore {
    dir: openat::Dir,
    _lock: File,
}

impl LockedStore {
    pub(super) fn open(root: &Path) -> Result<Self, CredentialError> {
        let root = open_root(root)?;
        validate_directory(&root, false)?;
        match root.create_dir("oauth", 0o700) {
            Ok(()) => root
                .sub_dir("oauth")
                .and_then(|dir| {
                    dir.open_file(".")?
                        .set_permissions(Permissions::from_mode(0o700))
                })
                .map_err(|_| failure("cannot secure OAuth directory"))?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(failure("cannot create OAuth directory")),
        }
        let dir = root
            .sub_dir("oauth")
            .map_err(|_| failure("OAuth directory must not be a symlink"))?;
        validate_directory(&dir, true)?;
        let lock = open_file(&dir, LOCK, libc::O_RDWR | libc::O_CREAT, 0o600)
            .map_err(|_| failure("cannot open OAuth lock"))?;
        validate_file(&lock)?;
        acquire(&lock, LOCK_WAIT)?;
        Ok(Self { dir, _lock: lock })
    }

    pub(super) fn read(&self) -> Result<Option<Vec<u8>>, CredentialError> {
        let file = match open_file(&self.dir, TOKEN, libc::O_RDONLY, 0) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(failure("cannot open token file; symlinks are refused")),
        };
        validate_file(&file)?;
        let mut bytes = Vec::new();
        file.take((MAX_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| failure("cannot read token file"))?;
        if bytes.len() > MAX_BYTES {
            return Err(failure("token file exceeds the byte limit"));
        }
        Ok(Some(bytes))
    }

    pub(super) fn write(&self, bytes: &[u8]) -> Result<(), CredentialError> {
        if bytes.len() > MAX_BYTES {
            return Err(failure("token file exceeds the byte limit"));
        }
        // Authenticate the existing destination under the same stable lock before replacing it.
        self.read()?;
        let name = format!(".codex.{}.tmp", std::process::id());
        let mut file = open_file(
            &self.dir,
            &name,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )
        .map_err(|_| failure("cannot create private token staging file"))?;
        let result = (|| {
            file.set_permissions(Permissions::from_mode(0o600))
                .map_err(|_| failure("cannot secure token staging file"))?;
            validate_file(&file)?;
            file.write_all(bytes)
                .and_then(|()| file.sync_all())
                .map_err(|_| failure("cannot sync token staging file"))?;
            self.dir
                .local_rename(&name, TOKEN)
                .map_err(|_| failure("cannot publish token file"))?;
            self.dir
                .open_file(".")
                .and_then(|file| file.sync_all())
                .map_err(|_| failure("cannot sync OAuth directory"))
        })();
        if result.is_err() {
            let _ = self.dir.remove_file(&name);
        }
        result
    }
}

fn open_root(path: &Path) -> Result<openat::Dir, CredentialError> {
    if !path.is_absolute() {
        return Err(failure("configuration root must be absolute"));
    }
    let mut dir = openat::Dir::open("/").map_err(|_| failure("cannot open filesystem root"))?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                match dir.create_dir(name, 0o700) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(_) => return Err(failure("cannot create configuration directory")),
                }
                dir = dir
                    .sub_dir(name)
                    .map_err(|_| failure("configuration path must not contain symlinks"))?;
            }
            _ => {
                return Err(failure(
                    "configuration path must not contain parent components",
                ))
            }
        }
    }
    Ok(dir)
}

fn validate_directory(dir: &openat::Dir, private: bool) -> Result<(), CredentialError> {
    let metadata = dir
        .open_file(".")
        .and_then(|file| file.metadata())
        .map_err(|_| failure("cannot inspect OAuth directory"))?;
    if !metadata.is_dir()
        || !owned(&metadata)
        || metadata.mode() & 0o022 != 0
        || (private && metadata.mode() & 0o7777 != 0o700)
    {
        return Err(failure(
            "OAuth directory must be owned by the current user and private (0700)",
        ));
    }
    Ok(())
}

fn validate_file(file: &File) -> Result<(), CredentialError> {
    let metadata = file
        .metadata()
        .map_err(|_| failure("cannot inspect OAuth file"))?;
    if !metadata.is_file()
        || !owned(&metadata)
        || metadata.mode() & 0o7777 != 0o600
        || metadata.nlink() != 1
    {
        return Err(failure("OAuth files must be single-link regular files owned by the current user with mode 0600"));
    }
    Ok(())
}

fn owned(metadata: &Metadata) -> bool {
    metadata.uid() == unsafe { libc::geteuid() }
}

fn open_file(
    dir: &openat::Dir,
    name: &str,
    flags: i32,
    mode: libc::mode_t,
) -> std::io::Result<File> {
    let name = CString::new(name).map_err(std::io::Error::other)?;
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            mode as libc::c_uint,
        )
    };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

fn acquire(file: &File, timeout: Duration) -> Result<(), CredentialError> {
    let start = Instant::now();
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if start.elapsed() >= timeout {
                    return Err(failure("OAuth store is busy; lock wait expired"));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return Err(failure("cannot lock OAuth store")),
        }
    }
}

fn failure(message: &str) -> CredentialError {
    CredentialError::local(message)
}

#[cfg(test)]
mod tests;
