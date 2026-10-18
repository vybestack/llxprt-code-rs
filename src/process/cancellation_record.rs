//! Pre-opened signal evidence: the handler only claims an fd and writes one byte.
use std::fs::File;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicI32, Ordering};

static RECORD_FD: AtomicI32 = AtomicI32::new(-1);
const CLAIMED: i32 = -2;

pub(crate) struct CancellationRecord(Option<File>);

impl CancellationRecord {
    pub(crate) fn install(file: File) -> Result<Self, String> {
        RECORD_FD
            .compare_exchange(-1, file.as_raw_fd(), Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| "a cancellation record is already installed".to_string())?;
        Ok(Self(Some(file)))
    }
}

impl Drop for CancellationRecord {
    fn drop(&mut self) {
        let file = self.0.take().expect("owned cancellation descriptor");
        if RECORD_FD.swap(-1, Ordering::SeqCst) == CLAIMED {
            // A handler on another thread owns the fd until _exit. Never close/reuse it
            // under pwrite; the process is already terminating.
            std::mem::forget(file);
        }
    }
}

pub(super) fn record(signal: libc::c_int) {
    let fd = RECORD_FD.load(Ordering::SeqCst);
    if fd < 0
        || RECORD_FD
            .compare_exchange(fd, CLAIMED, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
    {
        return;
    }
    let byte = if signal == libc::SIGINT { b'I' } else { b'T' };
    // Safety: the registration retains this regular-file descriptor through _exit.
    // pwrite is async-signal-safe; no allocation, formatting, locks or session IO.
    // Retry only EINTR; a storage failure must not prevent tool cleanup or signal exit.
    loop {
        let written = unsafe { libc::pwrite(fd, (&byte as *const u8).cast(), 1, 0) };
        if written >= 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            break;
        }
    }
}
