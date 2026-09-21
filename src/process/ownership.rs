//! A command scope has a group leader distinct from the command PID.
//!
//! The leader retains only a read end of an ownership pipe. The runtime retains the
//! sole writer; forked commands close it before exec. EOF (including SIGKILL of a
//! nested runtime) destroys the scope. Thus nested scopes cascade without trusting
//! environment PGIDs or enumerating descendants. Each command keeps an independent
//! group for its own timeout. This covers managed launches, not hostile daemons.
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::Command;

pub(super) struct Scope {
    pub(super) pgid: i32,
    writer: OwnedFd,
}

fn pipe() -> Result<(OwnedFd, OwnedFd), String> {
    let mut fds = [-1; 2];
    unsafe {
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        // Own both originals before any fallible operation. Protocol descriptors
        // must survive Command replacing descriptors 0..=2 during stdio setup.
        let mut pair = (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1]));
        for fd in [&mut pair.0, &mut pair.1] {
            let duplicate = libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3);
            if duplicate < 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            *fd = OwnedFd::from_raw_fd(duplicate);
        }
        Ok(pair)
    }
}

impl Scope {
    pub(super) fn new() -> Result<Self, String> {
        let (reader, writer) = pipe()?;
        let (ready_read, ready_write) = pipe()?;
        // Calculate before fork: the guardian executes only async-signal-safe libc
        // operations, never Rust allocation, logging, destructors or mutexes.
        let max_fd = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) };
        if max_fd < 0 {
            return Err("cannot determine guardian descriptor bound".into());
        }
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        if pid == 0 {
            unsafe {
                // Reset inherited runtime cancellation handlers before publication.
                libc::signal(libc::SIGTERM, libc::SIG_IGN);
                libc::signal(libc::SIGINT, libc::SIG_IGN);
                if libc::setpgid(0, 0) != 0 {
                    libc::_exit(126);
                }
                // In particular, don't retain writers belonging to ancestor scopes.
                for fd in 0..max_fd as i32 {
                    if fd != reader.as_raw_fd() && fd != ready_write.as_raw_fd() {
                        libc::close(fd);
                    }
                }
                let byte = [1_u8];
                if libc::write(ready_write.as_raw_fd(), byte.as_ptr().cast(), 1) != 1 {
                    libc::_exit(126);
                }
                libc::close(ready_write.as_raw_fd());
                let mut byte = [0_u8];
                loop {
                    let n = libc::read(reader.as_raw_fd(), byte.as_mut_ptr().cast(), 1);
                    if n < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR)
                    {
                        continue;
                    }
                    // No messages: EOF is irrevocable loss of the owner capability.
                    libc::kill(-libc::getpid(), libc::SIGKILL);
                    libc::_exit(0);
                }
            }
        }
        drop(reader);
        drop(ready_write);
        let scope = Self { pgid: pid, writer };
        let mut byte = [0_u8];
        loop {
            let n = unsafe { libc::read(ready_read.as_raw_fd(), byte.as_mut_ptr().cast(), 1) };
            if n == 1 {
                return Ok(scope);
            }
            if n < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err("command scope leader failed before publication".into());
        }
    }

    pub(super) fn configure(&self, cmd: &mut Command) {
        let pgid = self.pgid;
        let writer = self.writer.as_raw_fd();
        unsafe {
            cmd.pre_exec(move || {
                libc::close(writer);
                // Unlike setsid, this group belongs to the retained scope leader.
                // If ownership was lost before this hook, joining fails: no exec.
                if libc::setpgid(0, pgid) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-self.pgid, libc::SIGKILL);
            // Also handles failure before setpgid. PID cannot be reused until reaped.
            libc::kill(self.pgid, libc::SIGKILL);
            while libc::waitpid(self.pgid, std::ptr::null_mut(), 0) < 0 {
                if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                    break;
                }
            }
        }
    }
}
