//! Bounded subprocess runner shared by the shell tool (`run_shell_command`), the parity
//! CLI harness, and the grader.
//!
//! Every run gets its own process group/session (Unix `setsid`) so a timeout can signal
//! the whole tree, not just the direct child. The deadline starts the moment the child is
//! spawned. stdout and stderr are drained concurrently by two reader threads against a single
//! mutex-protected combined byte budget, so a flood on either pipe cannot deadlock the run and
//! the combined captured output is bounded with no underflow. When the capture cap is reached the
//! readers keep draining by discarding bytes, so a writer never fills its pipe.
//!
//! Captured output is raw bytes (never `from_utf8_lossy`); each stream records whether it was
//! truncated so a caller can choose its own encoding. Deadline supervision keeps going until the
//! child is reaped **and** both pipes have closed. If the pipes are still open past the
//! deadline (an escaped `setsid` descendant on Unix cannot be killed by a process-group signal),
//! the runner kills the original process group, aborts the readers (closing the local read ends)
//! and returns `timed_out`; joining the reader threads never blocks past a poll tick.
//!
//! The runner is Unix-only (macOS/Linux supported). Inherited environment is scrubbed: only
//! `PATH`, `HOME`, `TMPDIR`, `LANG`, `LC_*` and the caller's explicit additions are
//! passed through.

#[cfg(not(unix))]
compile_error!("llxprt-code-rs is Unix-only (macOS/Linux); the process runner relies on Unix setsid/poll/process-group machinery");

use std::io::Read;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Everything the runner needs to spawn and bound one command.
pub struct CmdSpec {
    pub program: String,
    pub args: Vec<String>,
    /// Pathname working directory for the child. Inherits the parent's when `None`.
    ///
    /// This is the **pathname** fallback used by the non-agent (grading/harness)
    /// callers. The agent shell path passes the retained workspace root via
    /// [`CmdSpec::cwd_fd`] instead, so the shell executes relative to the same
    /// retained directory the file tools use and never re-resolves (or canonicalizes) the
    /// cwd pathname.
    pub cwd: Option<PathBuf>,
    /// Unix directory descriptor the child runs in, taken from the retained workspace root.
    /// When `Some`, [`run_cmd`] calls `fchdir(fd)` between fork and exec without resolving a
    /// pathname, and `cwd` is ignored. The descriptor may close at exec because the cwd remains
    /// pinned independently. A failed `fchdir` fails the spawn before the command executes.
    pub cwd_fd: Option<i32>,
    /// Explicit extra environment variables added on top of the allow-list.
    pub env_add: Vec<(String, String)>,
    pub timeout: Duration,
    /// Combined cap for stdout + stderr in bytes.
    pub max_output: usize,
}

/// What one bounded run produced. `status` is the exit code, or `None` when the process
/// was terminated by a signal. `timed_out` is set when the deadline fired. `stdout` and
/// `stderr` are the raw captured bytes; each stream's `*_truncated` flag records whether
/// bytes were dropped because the combined budget was exhausted.
pub struct CmdOutcome {
    pub status: Option<i32>,
    pub timed_out: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub combined_truncated: bool,
}

/// Grace period between SIGTERM and SIGKILL when escalating a timed-out process group.
const TERM_GRACE: Duration = Duration::from_millis(200);
/// Poll tick shared by the reader threads so an abort is observed within this window.
const POLL_TICK_MS: i32 = 10;

/// Run `/bin/sh -c <command>` with the same bounds as [`run_cmd`].
pub fn run_sh(
    command: &str,
    cwd: Option<&Path>,
    timeout: Duration,
    max_output: usize,
    env_add: Vec<(String, String)>,
) -> Result<CmdOutcome, String> {
    run_cmd(CmdSpec {
        program: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), command.to_string()],
        cwd: cwd.map(|p| p.to_path_buf()),
        cwd_fd: None,
        env_add,
        timeout,
        max_output,
    })
}

/// Spawn, bound, and report on a command. Returns `Err` only if the spawn itself failed.
pub fn run_cmd(spec: CmdSpec) -> Result<CmdOutcome, String> {
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args);
    // Register setsid first, then fchdir. Both hooks run in order after fork and before exec.
    cfg_setsid(&mut cmd);
    if let Some(fd) = spec.cwd_fd {
        cmd_cwd_fd(&mut cmd, fd)?;
    } else if let Some(d) = &spec.cwd {
        cmd.current_dir(d);
    }
    scrub_env(&mut cmd, &spec.env_add);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (deadline, escalation_deadline) = command_deadlines(Instant::now(), spec.timeout)?;
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("spawn {} failed: {e}", spec.program))?;

    let budget = Arc::new(ByteBudget::new(spec.max_output));
    let abort = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicUsize::new(0));
    let mut pipes = 0usize;
    let mut handles = Vec::new();
    if let Some(so) = child.stdout.take() {
        pipes += 1;
        handles.push(drain_thread(
            so,
            budget.clone(),
            abort.clone(),
            done.clone(),
        ));
    }
    if let Some(se) = child.stderr.take() {
        pipes += 1;
        handles.push(drain_thread(
            se,
            budget.clone(),
            abort.clone(),
            done.clone(),
        ));
    }

    let (status, timed_out) = supervise(child, deadline, escalation_deadline, &done, pipes);

    // Abort makes every still-running reader exit at its next poll tick; threads that already saw
    // EOF are finished. Either way joining cannot block long.
    abort.store(true, Ordering::SeqCst);
    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    let mut stdout_truncated = false;
    let mut stderr_truncated = false;
    for (i, h) in handles.into_iter().enumerate() {
        if let Ok((bytes, truncated)) = h.join() {
            if i == 0 {
                stdout = bytes;
                stdout_truncated = truncated;
            } else {
                stderr = bytes;
                stderr_truncated = truncated;
            }
        }
    }
    let combined_truncated = stdout_truncated || stderr_truncated;
    Ok(CmdOutcome {
        status,
        timed_out,
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
        combined_truncated,
    })
}

/// A mutex-guarded combined byte budget. `take` never underflows: it returns the smaller of the
fn command_deadlines(start: Instant, timeout: Duration) -> Result<(Instant, Instant), String> {
    let deadline = start
        .checked_add(timeout)
        .ok_or_else(|| "command timeout cannot be represented".to_string())?;
    let escalation_deadline = deadline
        .checked_add(TERM_GRACE)
        .ok_or_else(|| "command termination deadline cannot be represented".to_string())?;
    Ok((deadline, escalation_deadline))
}

/// requested amount and the remaining budget.
struct ByteBudget {
    remaining: Mutex<usize>,
}

impl ByteBudget {
    fn new(limit: usize) -> Self {
        ByteBudget {
            remaining: Mutex::new(limit),
        }
    }

    fn take(&self, want: usize) -> usize {
        let mut g = self.remaining.lock().unwrap();
        let n = want.min(*g);
        *g -= n;
        n
    }
}

/// Set `O_NONBLOCK` on an fd so a read after a spurious `POLLIN` can never block the
/// reader past the abort tick.
fn set_nonblocking(fd: i32) {
    // Safety: `fcntl` on a valid fd; `F_GETFL`/`F_SETFL` are standard.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags >= 0 {
        let _ = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    }
}

/// Drain one pipe to EOF or abort. Bytes past the budget are read and discarded so the writer's
/// pipe never fills; the reader only stops at EOF or abort. Returns (captured bytes, truncated).
fn drain_thread<R: Read + AsRawFd + Send + 'static>(
    mut r: R,
    budget: Arc<ByteBudget>,
    abort: Arc<AtomicBool>,
    done: Arc<AtomicUsize>,
) -> std::thread::JoinHandle<(Vec<u8>, bool)> {
    std::thread::spawn(move || {
        set_nonblocking(r.as_raw_fd());
        let mut stored = Vec::new();
        let mut total = 0usize;
        let mut buf = vec![0u8; 16 * 1024];
        let mut pollfd = libc::pollfd {
            fd: r.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            if abort.load(Ordering::SeqCst) {
                break;
            }
            // Safety: `poll` on a valid open fd with a timeout is safe.
            let rc = unsafe { libc::poll(&mut pollfd, 1, POLL_TICK_MS) };
            if rc == 0 {
                continue;
            }
            if rc < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
            let ev = pollfd.revents;
            if ev & (libc::POLLNVAL | libc::POLLERR) != 0
                && ev & (libc::POLLIN | libc::POLLHUP) == 0
            {
                break;
            }
            match r.read(&mut buf) {
                Ok(0) => break, // EOF: every writer closed
                Ok(n) => {
                    total = total.saturating_add(n);
                    let take = budget.take(n);
                    stored.extend_from_slice(&buf[..take]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        done.fetch_add(1, Ordering::SeqCst);
        let truncated = stored.len() < total;
        (stored, truncated)
    })
}

/// Supervise the child until it has exited **and** every pipe has closed. Exit is observed with
/// `waitid(WNOWAIT)`, so the child remains a zombie and continues to reserve its PID while that PID
/// is still used as the process-group ID for TERM/KILL. The child is reaped only after no more group
/// signal can be sent. This prevents an escaped pipe holder from turning PID reuse into a signal to
/// an unrelated process group.
fn supervise(
    mut child: Child,
    deadline: Instant,
    escalation_deadline: Instant,
    done: &AtomicUsize,
    pipes: usize,
) -> (Option<i32>, bool) {
    let pid = child.id();
    let mut exited = false;
    let mut passed_deadline = false;
    let mut term_sent = false;
    loop {
        if !exited {
            match child_exited_unreaped(pid) {
                Ok(observed) => exited = observed,
                Err(_) => {
                    let _ = kill_group(&mut child, libc::SIGKILL);
                    return (child.wait().ok().and_then(|status| status.code()), true);
                }
            }
        }
        let closed = done.load(Ordering::SeqCst) >= pipes;
        let now = Instant::now();
        if now >= deadline {
            passed_deadline = true;
        }
        if exited && closed && !passed_deadline {
            return (child.wait().ok().and_then(|status| status.code()), false);
        }
        // A timed-out direct child may exit and close its pipes while a TERM-ignoring descendant
        // remains in the same process group with redirected stdio. Always complete the group-wide
        // escalation before reaping the retained leader and returning from a timeout.
        if now >= escalation_deadline {
            let _ = kill_group(&mut child, libc::SIGKILL);
            return (child.wait().ok().and_then(|status| status.code()), true);
        }
        if passed_deadline && !term_sent {
            let _ = kill_group(&mut child, libc::SIGTERM);
            term_sent = true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Observe child exit without consuming its wait status or releasing its PID.
fn child_exited_unreaped(pid: u32) -> std::io::Result<bool> {
    let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            info.as_mut_ptr(),
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let info = unsafe { info.assume_init() };
    Ok(unsafe { info.si_pid() } == pid as libc::pid_t)
}

/// Scrub inherited env, allowing only PATH/HOME/TMPDIR/LANG/LC_* plus explicit entries.
fn scrub_env(cmd: &mut Command, add: &[(String, String)]) {
    cmd.env_clear();
    for (k, v) in std::env::vars() {
        if allow_key(&k) {
            cmd.env(k, v);
        }
    }
    for (k, v) in add {
        cmd.env(k, v);
    }
}

fn allow_key(k: &str) -> bool {
    matches!(k, "PATH" | "HOME" | "TMPDIR" | "LANG") || k.starts_with("LC_")
}

/// Put the child in its own session/process group so its pid is the group id and a
/// negative-pid kill reaches every descendant.
fn cfg_setsid(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    // Safety: `pre_exec` runs between fork and exec in the child where `setsid` is safe; it
    // creates a new session whose process group id equals the child's pid.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// Bind the child's working directory to the retained workspace descriptor between fork and
/// exec. The cwd remains pinned after a close-on-exec descriptor closes, so the descriptor does
/// not need to survive exec. No pathname lookup occurs in the child.
fn cmd_cwd_fd(cmd: &mut Command, fd: i32) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    if fd < 0 {
        return Err("workspace descriptor is invalid".to_string());
    }
    // Safety: `fd` is retained by the parent for the command lifetime. This hook runs after the
    // setsid hook in the forked child; an fchdir failure aborts the spawn before exec.
    unsafe {
        cmd.pre_exec(move || {
            if libc::fchdir(fd) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(())
}

/// Signal the child's whole process group (negative pid). Falls back to killing the direct child.
fn kill_group(child: &mut Child, signum: i32) -> Result<(), String> {
    // Safety: -pid is the new session's process group id (== child pid via `setsid`).
    if unsafe { libc::kill(-(child.id() as i32), signum) } == 0 {
        return Ok(());
    }
    child.kill().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn duration_from_nanos(nanos: u128) -> Duration {
        Duration::new(
            (nanos / 1_000_000_000) as u64,
            (nanos % 1_000_000_000) as u32,
        )
    }

    #[test]
    fn exit_observation_preserves_wait_status_until_explicit_reap() {
        let mut child = Command::new("sh").arg("-c").arg("exit 7").spawn().unwrap();
        let pid = child.id();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !child_exited_unreaped(pid).unwrap() {
            assert!(Instant::now() < deadline, "child did not become waitable");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(child.wait().unwrap().code(), Some(7));
    }

    #[test]
    fn signaling_unreaped_group_never_reaches_unrelated_sentinel() {
        let mut exited_command = Command::new("sh");
        exited_command.arg("-c").arg("exit 0");
        cfg_setsid(&mut exited_command);
        let mut exited = exited_command.spawn().unwrap();
        let exited_pid = exited.id();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !child_exited_unreaped(exited_pid).unwrap() {
            assert!(Instant::now() < deadline, "child did not become waitable");
            std::thread::sleep(Duration::from_millis(1));
        }

        let mut sentinel_command = Command::new("sh");
        sentinel_command.arg("-c").arg("sleep 30");
        cfg_setsid(&mut sentinel_command);
        let mut sentinel = sentinel_command.spawn().unwrap();
        kill_group(&mut exited, libc::SIGTERM).unwrap();
        assert!(
            sentinel.try_wait().unwrap().is_none(),
            "signal for the retained zombie group reached an unrelated process"
        );

        assert_eq!(exited.wait().unwrap().code(), Some(0));
        let _ = kill_group(&mut sentinel, libc::SIGKILL);
        let _ = sentinel.wait();
    }

    #[test]
    fn timeout_sweeps_descendant_after_direct_child_exits_and_closes_pipes() {
        let directory = tempfile::tempdir().unwrap();
        let pidfile = directory.path().join("descendant.pid");
        let script = format!(
            "trap 'exit 0' TERM; (trap '' TERM; exec sleep 30) </dev/null >/dev/null 2>&1 & echo $! > '{}'; while :; do :; done",
            pidfile.display()
        );
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(script)
            .current_dir(directory.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        cfg_setsid(&mut command);
        let child = command.spawn().unwrap();

        let ready_deadline = Instant::now() + Duration::from_secs(2);
        while !pidfile.is_file() {
            assert!(
                Instant::now() < ready_deadline,
                "direct child did not report descendant readiness"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        let deadline = Instant::now() + Duration::from_millis(50);
        let escalation_deadline = deadline + TERM_GRACE;
        let done = AtomicUsize::new(0);
        let (status, timed_out) = supervise(child, deadline, escalation_deadline, &done, 0);
        assert!(timed_out);
        assert_eq!(status, Some(0));

        let descendant: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            // Safety: signal zero only probes whether the recorded process still exists.
            let probe = unsafe { libc::kill(descendant, 0) };
            if probe == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "TERM-ignoring same-group descendant survived timeout cleanup"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn grace_overflow_is_rejected_without_panicking() {
        let start = Instant::now();
        let mut low = 0u128;
        let mut high = u64::MAX as u128 * 1_000_000_000 + 999_999_999;
        while low < high {
            let middle = low + (high - low).div_ceil(2);
            if start.checked_add(duration_from_nanos(middle)).is_some() {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        let error = command_deadlines(start, duration_from_nanos(low)).unwrap_err();
        assert_eq!(error, "command termination deadline cannot be represented");
    }
}
