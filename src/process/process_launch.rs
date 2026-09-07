//! Fork/exec launch-to-cancellation publication handoff.

use super::ACTIVE_GROUP;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::process::Command;
use std::sync::atomic::Ordering;

/// Pipe endpoints that make exec contingent on a parent-side publication. The child reports its
/// post-`setsid` pid then waits for an acknowledgement; the coordinator stores that pid in
/// `ACTIVE_GROUP` before acknowledging. If cancellation exits the worker at any earlier point,
/// closing the acknowledgement writer makes the child fail its pre-exec hook rather than exec.
pub(super) struct LaunchHandoff {
    child_ready_write: OwnedFd,
    child_ack_read: OwnedFd,
    // These are owned by the coordinator thread in the parent. Their raw values are retained
    // solely so the post-fork child can close its inherited copies before it can wait on them.
    parent_ready_read: RawFd,
    parent_ack_write: RawFd,
    coordinator: std::thread::JoinHandle<()>,
    #[cfg(test)]
    test_gate: Option<TestLaunchGate>,
}

impl LaunchHandoff {
    pub(super) fn new() -> Result<Self, String> {
        let (parent_ready_read, child_ready_write) =
            cloexec_pipe().map_err(|error| format!("create launch-ready pipe failed: {error}"))?;
        let (child_ack_read, parent_ack_write) = cloexec_pipe()
            .map_err(|error| format!("create launch-acknowledgement pipe failed: {error}"))?;
        #[cfg(test)]
        let test_gate = if std::env::var_os("LLXPRT_TEST_LAUNCH_READY").is_some() {
            Some(TestLaunchGate::new()?)
        } else {
            None
        };
        let parent_ready_fd = parent_ready_read.as_raw_fd();
        let parent_ack_fd = parent_ack_write.as_raw_fd();
        Ok(Self {
            child_ready_write,
            child_ack_read,
            parent_ready_read: parent_ready_fd,
            parent_ack_write: parent_ack_fd,
            coordinator: std::thread::spawn(move || {
                launch_coordinator(parent_ready_read, parent_ack_write);
            }),
            #[cfg(test)]
            test_gate,
        })
    }

    pub(super) fn finish(self) {
        // On a pre-exec failure no child writes the ready pipe. Drop the inherited child ends
        // first, so the coordinator observes EOF rather than remaining blocked in `read`.
        let Self {
            child_ready_write,
            child_ack_read,
            coordinator,
            #[cfg(test)]
            test_gate,
            ..
        } = self;
        drop(child_ready_write);
        drop(child_ack_read);
        #[cfg(test)]
        drop(test_gate);
        let _ = coordinator.join();
    }

    pub(super) fn parent_ends_in_child(&self) -> (RawFd, RawFd) {
        (self.parent_ready_read, self.parent_ack_write)
    }

    #[cfg(test)]
    pub(super) fn test_gate_parent_write(&self) -> Option<RawFd> {
        self.test_gate
            .as_ref()
            .map(|gate| gate.parent_write.as_raw_fd())
    }

    #[cfg(test)]
    pub(super) fn test_gate_read(&self) -> Option<RawFd> {
        self.test_gate
            .as_ref()
            .map(|gate| gate.child_read.as_raw_fd())
    }
}

/// The coordinator is intentionally a normal thread, not the signal handler: all it does is
/// turn the child-provided pid into the published cancellation target and release the child.
/// Process exit closes its acknowledgement fd, which is the cancellation path before publication.
fn launch_coordinator(ready: OwnedFd, acknowledgement: OwnedFd) {
    let mut pid_bytes = [0_u8; std::mem::size_of::<i32>()];
    if read_all(ready.as_raw_fd(), &mut pid_bytes).is_err() {
        return;
    }
    let pgid = i32::from_ne_bytes(pid_bytes);
    if pgid <= 0 {
        return;
    }
    ACTIVE_GROUP.store(pgid, Ordering::SeqCst);
    let _ = write_all(acknowledgement.as_raw_fd(), &[1]);
    // Explicit drops document that the child sees EOF if it has not consumed the acknowledgement.
    drop(ready);
    drop(acknowledgement);
}

/// Close parent-only endpoints inherited across fork. Without this, a cancelled worker could
/// leave the child holding its own acknowledgement writer, turning the required EOF into a hang.
pub(super) fn cfg_launch_close_parent_ends(
    cmd: &mut Command,
    parent_ends: (RawFd, RawFd),
    #[cfg(test)] test_parent_end: Option<RawFd>,
) {
    use std::os::unix::process::CommandExt;
    // Safety: only close descriptors the child inherited from this launch; they remain owned by
    // the coordinator in the parent. This hook is ordered before every possible child wait.
    unsafe {
        cmd.pre_exec(move || {
            libc::close(parent_ends.0);
            libc::close(parent_ends.1);
            #[cfg(test)]
            if let Some(fd) = test_parent_end {
                libc::close(fd);
            }
            Ok(())
        });
    }
}

/// Install the child half of the launch handoff after `setsid`: report the new group id and do
/// not return from pre-exec until the coordinator has made it cancellable.
pub(super) fn cfg_launch_handoff(cmd: &mut Command, handoff: &LaunchHandoff) {
    use std::os::unix::process::CommandExt;
    let ready_write = handoff.child_ready_write.as_raw_fd();
    let acknowledgement_read = handoff.child_ack_read.as_raw_fd();
    // Safety: the hook performs only async-signal-safe syscalls in the forked child. A zero-byte
    // acknowledgement is EOF from a cancelling/exited worker and aborts `spawn` before exec.
    unsafe {
        cmd.pre_exec(move || {
            let pid = (libc::getpid() as i32).to_ne_bytes();
            if write_all(ready_write, &pid).is_err() {
                return Err(std::io::Error::from_raw_os_error(libc::EPIPE));
            }
            libc::close(ready_write);
            let mut acknowledgement = [0_u8; 1];
            if read_all(acknowledgement_read, &mut acknowledgement).is_err() {
                return Err(std::io::Error::from_raw_os_error(libc::ENODATA));
            }
            libc::close(acknowledgement_read);
            Ok(())
        });
    }
}

/// A pipe whose writer remains in the worker only for the deterministic launch-race regression.
/// The child closes its inherited writer before the hook waits for EOF from worker cancellation.
#[cfg(test)]
struct TestLaunchGate {
    child_read: OwnedFd,
    parent_write: OwnedFd,
}

#[cfg(test)]
impl TestLaunchGate {
    pub(super) fn new() -> Result<Self, String> {
        let (child_read, parent_write) = cloexec_pipe()
            .map_err(|error| format!("create launch test-gate pipe failed: {error}"))?;
        Ok(Self {
            child_read,
            parent_write,
        })
    }
}

/// Test-only pre-exec gate. It writes the marker after `setsid`, then waits for the worker's fd
/// to close. The parent test sends cancellation only after observing the marker, so this pins the
/// signal before `ACTIVE_GROUP` publication without timing or retry assumptions.
#[cfg(test)]
pub(super) fn cfg_launch_test_barrier(cmd: &mut Command, test_gate: Option<RawFd>) {
    use std::ffi::CString;
    use std::os::unix::{ffi::OsStrExt, process::CommandExt};

    let Some(marker) = std::env::var_os("LLXPRT_TEST_LAUNCH_READY") else {
        return;
    };
    let gate_read = test_gate.expect("launch test marker has a gate");
    let marker = CString::new(marker.as_bytes()).expect("launch test marker contains no NUL");
    // Safety: syscall-only work in the post-fork child. This hook precedes the launch handoff.
    unsafe {
        cmd.pre_exec(move || {
            let fd = libc::open(
                marker.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                0o600,
            );
            if fd == -1 {
                return Err(std::io::Error::last_os_error());
            }
            let mut pid = libc::getpid() as u32;
            let mut digits = [0_u8; 10];
            let mut start = digits.len();
            loop {
                start -= 1;
                digits[start] = b'0' + (pid % 10) as u8;
                pid /= 10;
                if pid == 0 {
                    break;
                }
            }
            let written = write_all(fd, &digits[start..]);
            libc::close(fd);
            if written.is_err() {
                return Err(std::io::Error::last_os_error());
            }
            let mut cancellation = [0_u8; 1];
            if read_all(gate_read, &mut cancellation).is_ok() {
                return Err(std::io::Error::from_raw_os_error(libc::ECANCELED));
            }
            libc::close(gate_read);
            Err(std::io::Error::last_os_error())
        });
    }
}

/// Make a close-on-exec pipe. The pre-exec hooks still use its descriptors, while successful exec
/// cannot leak them into the tool.
fn cloexec_pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [-1; 2];
    // Safety: `fds` points to two writable descriptor slots.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    // `Command` builds its stdio and exec-status plumbing after the hooks are registered. Keep
    // our protocol far above its low-numbered descriptors so child-side setup cannot reuse a raw
    // endpoint a hook is about to close or read.
    for fd in &mut fds {
        // Safety: duplicate the newly-created descriptor to the lowest available fd >= 64, then
        // release the low-numbered original. `F_DUPFD` is available on both supported Unix hosts.
        let duplicated = unsafe { libc::fcntl(*fd, libc::F_DUPFD, 64) };
        if duplicated == -1 {
            let error = std::io::Error::last_os_error();
            unsafe {
                libc::close(fds[0]);
                libc::close(fds[1]);
            }
            return Err(error);
        }
        unsafe { libc::close(*fd) };
        *fd = duplicated;
        // Safety: the duplicate is ours and must not survive a successful exec.
        if unsafe { libc::fcntl(*fd, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
            let error = std::io::Error::last_os_error();
            unsafe {
                libc::close(fds[0]);
                libc::close(fds[1]);
            }
            return Err(error);
        }
    }
    // Safety: each high-numbered raw fd is owned exactly once after setup.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// Retry interrupted pipe syscalls. EOF is an error so a child never mistakes a dead worker for
/// permission to exec.
fn read_all(fd: RawFd, bytes: &mut [u8]) -> Result<(), ()> {
    let mut offset = 0;
    while offset < bytes.len() {
        // Safety: the slice supplies `bytes.len() - offset` writable bytes.
        let read = unsafe {
            libc::read(
                fd,
                bytes[offset..].as_mut_ptr().cast(),
                bytes.len() - offset,
            )
        };
        if read > 0 {
            offset += read as usize;
        } else if read == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR)
        {
            continue;
        } else {
            return Err(());
        }
    }
    Ok(())
}

/// Retry interrupted pipe writes. Short writes are completed before the child waits for an ack.
fn write_all(fd: RawFd, bytes: &[u8]) -> Result<(), ()> {
    let mut offset = 0;
    while offset < bytes.len() {
        // Safety: the slice supplies `bytes.len() - offset` readable bytes.
        let written =
            unsafe { libc::write(fd, bytes[offset..].as_ptr().cast(), bytes.len() - offset) };
        if written > 0 {
            offset += written as usize;
        } else if written == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR)
        {
            continue;
        } else {
            return Err(());
        }
    }
    Ok(())
}
