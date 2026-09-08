//! Linux mechanism witness for the wrapper-writer topology change (issue #264).
//!
//! Linux refuses to `exec` a file whose inode still has a writable descriptor open
//! anywhere (`ETXTBSY`). The old test topology opened that writer in the library test
//! process, so a sibling test child forked while the writer was open inherited a
//! writable reference to the wrapper's inode and held it until its own `exec` or `exit`:
//! `O_CLOEXEC` closes on exec, so it is no fork barrier. The repair is that each whole
//! wrapper sequence now runs in one dedicated helper process, so no library-test sibling
//! can be forked while that writer is open (see `tests_inject.rs`).
//!
//! This is a mirrored mechanism witness, not an attribution of the historical CI failure:
//! no prior run recorded which pid held the descriptor, so the red half reproduces the
//! mechanism itself — open, write, and `chmod` a real wrapper through one descriptor,
//! fork a sibling while that writer is open and hold it before its first `exec`, close
//! this process's writer, then one direct `exec` that Linux refuses with `ETXTBSY` — and
//! the green half holds a comparable sibling while the real `tests_inject`
//! `fault_sequence_helper` runs the restart-after-round-2 sequence in its own process,
//! the only process that ever opens that wrapper's writer. Both halves run inside one
//! separately exec'd helper process, every rendezvous is bounded by `poll(2)`, the forked
//! sibling's whole body is async-signal-safe, and every child is released and reaped on
//! the passing path and killed and reaped on a failing one.

#[cfg(target_os = "linux")]
use crate::context_eval::{
    faults,
    tests_inject::{supervise_helper, HelperRun, FAULT_SEQUENCE_DIR, FAULT_SEQUENCE_HELPER},
};

#[cfg(target_os = "linux")]
use std::{
    fs,
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

/// Environment selector that turns this test binary into the witness helper process.
#[cfg(target_os = "linux")]
const WITNESS_HELPER: &str = "LLXPRT_TEST_ETXTBSY_HELPER";

/// Bound for every witness rendezvous: a report, a release, or an exit that never arrives
/// fails the witness instead of hanging it.
#[cfg(target_os = "linux")]
const WITNESS_BOUND: Duration = Duration::from_secs(120);

/// Fixed-width report a held sibling writes between its own `fork` and its first `exec`:
/// `[present:1][pid:4][fd:4][dev:8][ino:8][F_GETFD:8][F_GETFL:8]`, every field big-endian.
/// Byte `0` is the sibling's own "nothing inherited" word.
#[cfg(target_os = "linux")]
const REPORT_LEN: usize = 41;

/// The identity of one descriptor as the process holding it reports it.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Inherited {
    /// The reporting process's own pid.
    pid: u32,
    /// The descriptor number, which `fork` preserves.
    fd: RawFd,
    dev: u64,
    ino: u64,
    /// `F_GETFD`: descriptor flags, `FD_CLOEXEC` among them.
    fd_flags: i64,
    /// `F_GETFL`, masked to `O_ACCMODE`.
    acc_mode: i64,
}

#[cfg(target_os = "linux")]
impl Inherited {
    /// This process's own view of `fd`, to compare field for field with the forked
    /// sibling's report of the descriptor it inherited.
    fn of(pid: u32, fd: RawFd) -> Self {
        let mut stat = std::mem::MaybeUninit::uninit();
        // Safety: `stat` is the out-buffer of `fstat` on this process's own descriptor.
        assert_eq!(
            unsafe { libc::fstat(fd, stat.as_mut_ptr()) },
            0,
            "fstat fd {fd}: {}",
            std::io::Error::last_os_error()
        );
        let stat = unsafe { stat.assume_init() };
        // Safety: both `fcntl` calls read this process's own open descriptor.
        Self {
            pid,
            fd,
            dev: stat.st_dev as u64,
            ino: stat.st_ino as u64,
            fd_flags: unsafe { libc::fcntl(fd, libc::F_GETFD) } as i64,
            acc_mode: (unsafe { libc::fcntl(fd, libc::F_GETFL) } & libc::O_ACCMODE) as i64,
        }
    }

    /// Whether `F_GETFL` reports a writable access mode (`O_WRONLY` or `O_RDWR`).
    fn writable(&self) -> bool {
        self.acc_mode & libc::O_WRONLY as i64 != 0
    }

    /// Whether `F_GETFD` reports `FD_CLOEXEC`, which closes on exec but not on fork.
    fn cloexec(&self) -> bool {
        self.fd_flags & libc::FD_CLOEXEC as i64 != 0
    }
}

/// Decode the sibling's fixed-width report; `None` is its own "nothing inherited" word.
#[cfg(target_os = "linux")]
fn decode(report: &[u8; REPORT_LEN]) -> Option<Inherited> {
    if report[0] == 0 {
        return None;
    }
    let word = |offset: usize| u64::from_be_bytes(report[offset..offset + 8].try_into().unwrap());
    Some(Inherited {
        pid: u32::from_be_bytes(report[1..5].try_into().unwrap()),
        fd: u32::from_be_bytes(report[5..9].try_into().unwrap()) as RawFd,
        dev: word(9),
        ino: word(17),
        fd_flags: word(25) as i64,
        acc_mode: word(33) as i64,
    })
}

/// One `pipe` with `FD_CLOEXEC` set on both ends, as owned descriptors.
#[cfg(target_os = "linux")]
fn cloexec_pipe() -> (OwnedFd, OwnedFd) {
    let mut fds = [-1 as RawFd; 2];
    // Safety: `fds` supplies two writable descriptor slots.
    assert_eq!(
        unsafe { libc::pipe(fds.as_mut_ptr()) },
        0,
        "pipe failed: {}",
        std::io::Error::last_os_error()
    );
    for fd in fds {
        // Safety: both descriptors were created above and are owned by this pair.
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) },
            0,
            "set FD_CLOEXEC on {fd}: {}",
            std::io::Error::last_os_error()
        );
    }
    // Safety: each endpoint is owned exactly once, by one half of the pair.
    let pair = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    pair
}

/// Read exactly `out.len()` bytes, deadline-bounded by `poll(2)`: a peer that never writes
/// fails the assertion instead of hanging the witness. `false` is a clean EOF.
#[cfg(target_os = "linux")]
fn read_bounded(fd: RawFd, out: &mut [u8], what: &str) -> bool {
    let deadline = Instant::now() + WITNESS_BOUND;
    let mut filled = 0;
    while filled < out.len() {
        let timeout = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(i32::MAX as u128) as i32;
        let mut pending = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // Safety: `pending` describes this process's own open descriptor.
        let ready = unsafe { libc::poll(&mut pending, 1, timeout) };
        if ready == -1 {
            let error = std::io::Error::last_os_error();
            assert!(
                error.kind() == std::io::ErrorKind::Interrupted,
                "poll for {what}: {error}"
            );
            continue;
        }
        assert!(ready != 0, "timed out waiting for {what}");
        // Safety: the caller supplies `out.len() - filled` writable bytes.
        let read = unsafe { libc::read(fd, out[filled..].as_mut_ptr().cast(), out.len() - filled) };
        if read == -1 {
            let error = std::io::Error::last_os_error();
            assert!(
                error.kind() == std::io::ErrorKind::Interrupted,
                "read {what}: {error}"
            );
            continue;
        }
        if read == 0 {
            return false;
        }
        filled += read as usize;
    }
    true
}

/// The forked sibling's whole body: report the descriptor it inherited, then hold.
///
/// # Safety
///
/// This runs in the forked child of a process that may hold other threads, so every call
/// here is async-signal-safe and nothing can allocate, lock, or unwind: raw
/// `fstat`/`fcntl`/`getpid`/`write`/`read` into fixed stack buffers, then `_exit`. The
/// single `write` is `REPORT_LEN` bytes, far below `PIPE_BUF`, so it is atomic; the single
/// `read` blocks until the owner releases the sibling, which is the hold itself. A peer
/// that fails either step is reported by exiting nonzero, which the owner's bounded
/// rendezvous turns into a failing assertion.
#[cfg(target_os = "linux")]
unsafe fn held_sibling_body(witness: RawFd, report: RawFd, release: RawFd) -> ! {
    let mut bytes = [0_u8; REPORT_LEN];
    let mut stat = std::mem::MaybeUninit::uninit();
    // Safety: raw syscalls into fixed buffers only; see this function's contract.
    if unsafe { libc::fstat(witness, stat.as_mut_ptr()) } == 0 {
        let stat = unsafe { stat.assume_init() };
        bytes[0] = 1;
        bytes[1..5].copy_from_slice(&(unsafe { libc::getpid() } as u32).to_be_bytes());
        bytes[5..9].copy_from_slice(&(witness as u32).to_be_bytes());
        bytes[9..17].copy_from_slice(&(stat.st_dev as u64).to_be_bytes());
        bytes[17..25].copy_from_slice(&(stat.st_ino as u64).to_be_bytes());
        let fd_flags = unsafe { libc::fcntl(witness, libc::F_GETFD) } as u64;
        let acc_mode = (unsafe { libc::fcntl(witness, libc::F_GETFL) } & libc::O_ACCMODE) as u64;
        bytes[25..33].copy_from_slice(&fd_flags.to_be_bytes());
        bytes[33..41].copy_from_slice(&acc_mode.to_be_bytes());
    }
    // Safety: one atomic write of the whole fixed report into the owner's pipe.
    if unsafe { libc::write(report, bytes.as_ptr().cast(), bytes.len()) } != bytes.len() as isize {
        unsafe { libc::_exit(1) };
    }
    // Hold here, between this sibling's own `fork` and its first `exec`, until released.
    let mut byte = [0_u8; 1];
    // Safety: one blocking read of the single release byte.
    if unsafe { libc::read(release, byte.as_mut_ptr().cast(), 1) } != 1 {
        unsafe { libc::_exit(1) };
    }
    unsafe { libc::_exit(0) }
}

/// A sibling forked by the process that owns a writer, held between its own `fork` and its
/// first `exec`. It never execs: it reports and blocks, which is exactly the state a
/// forked-but-not-yet-exec'd library-test child is in, so the inherited descriptors stay
/// open for as long as the owner keeps it held.
#[cfg(target_os = "linux")]
struct HeldSibling {
    pid: u32,
    /// What the sibling reported about `witness`; `None` is its own "nothing inherited".
    inherited: Option<Inherited>,
    release: OwnedFd,
    report: OwnedFd,
    /// Set once the sibling is reaped, so a failing path cannot kill a reissued pid.
    released: bool,
}

#[cfg(target_os = "linux")]
impl HeldSibling {
    /// Fork the sibling from this process while `witness` is open in it, then read (bounded)
    /// the report its own body writes.
    fn hold(witness: RawFd, what: &str) -> Self {
        let (report_read, report_write) = cloexec_pipe();
        let (release_read, release_write) = cloexec_pipe();
        // Safety: the forked child only closes its two unused pipe ends and then runs
        // `held_sibling_body`, whose whole body is async-signal-safe (see its contract).
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork {what}: {}", std::io::Error::last_os_error());
        if pid == 0 {
            // Safety: the child's copies of the ends it does not use; raw `close` only.
            unsafe { libc::close(report_read.as_raw_fd()) };
            unsafe { libc::close(release_write.as_raw_fd()) };
            // Safety: this is the forked child, and `witness`, `report_write`, and
            // `release_read` are its own inherited descriptors.
            unsafe {
                held_sibling_body(witness, report_write.as_raw_fd(), release_read.as_raw_fd())
            };
        }
        // The parent keeps the report's read end and the release's write end. Closing its
        // copies of the child's ends here is what makes the report pipe's EOF the
        // sibling's exit rendezvous below.
        drop(report_write);
        drop(release_read);
        let mut raw = [0_u8; REPORT_LEN];
        assert!(
            read_bounded(report_read.as_raw_fd(), &mut raw, &format!("{what} report")),
            "{what} closed its report pipe before reporting"
        );
        Self {
            pid: pid as u32,
            inherited: decode(&raw),
            release: release_write,
            report: report_read,
            released: false,
        }
    }

    /// Release the sibling and reap it: it returns from its blocking read and `_exit`s,
    /// which closes the last write end of the report pipe, so the bounded EOF below is the
    /// reap rendezvous and the `waitpid` after it cannot block.
    fn release_and_reap(&mut self, what: &str) {
        // Safety: one byte into this process's own pipe end, whose reader is held.
        let written = unsafe { libc::write(self.release.as_raw_fd(), [1_u8].as_ptr().cast(), 1) };
        assert_eq!(
            written,
            1,
            "release {what}: {}",
            std::io::Error::last_os_error()
        );
        let mut byte = [0_u8; 1];
        assert!(
            !read_bounded(self.report.as_raw_fd(), &mut byte, &format!("{what} exit")),
            "{what} was still writing after its release"
        );
        self.released = true;
        let mut status = 0 as libc::c_int;
        // Safety: `self.pid` is this process's own unreaped child.
        let reaped = unsafe { libc::waitpid(self.pid as libc::pid_t, &mut status, 0) };
        assert_eq!(
            reaped as u32,
            self.pid,
            "reap {what}: {}",
            std::io::Error::last_os_error()
        );
        // Kept out of the assertion so the analyzer measures the branch, not a macro's
        // unexpanded tokens.
        let exited_cleanly = libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0;
        assert!(exited_cleanly, "{what} exited with status {status}");
    }
}

#[cfg(target_os = "linux")]
impl Drop for HeldSibling {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        // A failing path must not leave a held fork behind: kill it, then reap the corpse
        // so no orphan and no zombie outlives the witness.
        // Safety: `self.pid` is this process's own child, held or already dead.
        unsafe { libc::kill(self.pid as libc::pid_t, libc::SIGKILL) };
        let mut status = 0 as libc::c_int;
        // Safety: SIGKILL has been sent to this process's own child.
        let _ = unsafe { libc::waitpid(self.pid as libc::pid_t, &mut status, 0) };
    }
}

/// Whether `/proc/<pid>/fd` shows an open descriptor on exactly `(dev, ino)`, read from
/// this process while the sibling is held: the real descriptor set of the real pid, not a
/// sentinel. At least one descriptor must be visible, so an unreadable `/proc` entry fails
/// the witness instead of proving absence by silence.
#[cfg(target_os = "linux")]
fn holds_descriptor(pid: u32, dev: u64, ino: u64, what: &str) -> bool {
    let entries = fs::read_dir(format!("/proc/{pid}/fd"))
        .unwrap_or_else(|error| panic!("read /proc/{pid}/fd for {what}: {error}"));
    let mut visible = 0;
    let mut held = false;
    for entry in entries.flatten() {
        // Safety-free `stat` through the magic symlink: the target of an open descriptor.
        let Ok(target) = fs::metadata(entry.path()) else {
            continue;
        };
        visible += 1;
        // Kept out of the comparison itself so the analyzer measures this branch rather
        // than an unexpanded macro token stream.
        let same_descriptor = target.dev() == dev && target.ino() == ino;
        held |= same_descriptor;
    }
    assert!(
        visible > 0,
        "no descriptors were visible for {what} in /proc/{pid}/fd"
    );
    held
}

/// Print one causal evidence line: the real identity Linux acted on.
#[cfg(target_os = "linux")]
fn report_identity(half: &str, what: &str, id: &Inherited) {
    println!(
        "#264 {half}: {what}: pid={} fd={} dev={} ino={} F_GETFL={} (writable={}) \
         F_GETFD={} (FD_CLOEXEC={})",
        id.pid,
        id.fd,
        id.dev,
        id.ino,
        id.acc_mode,
        id.writable(),
        id.fd_flags,
        id.cloexec()
    );
}

/// Print the one refused exec with its real errno, next to the pid that refused it.
#[cfg(target_os = "linux")]
fn report_refusal(half: &str, error: &std::io::Error) {
    println!(
        "#264 {half}: one direct wrapper exec refused: errno={} ({error}) witness_pid={}",
        error.raw_os_error().unwrap_or_default(),
        std::process::id()
    );
}

/// Label the witness with the kernel that actually produced the evidence.
#[cfg(target_os = "linux")]
fn witness_header() {
    let release = fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|text| text.trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    println!(
        "#264 mirrored mechanism witness (not the historical CI interleaving): os={} \
         osrelease={} arch={} witness_pid={}",
        std::env::consts::OS,
        release,
        std::env::consts::ARCH,
        std::process::id()
    );
}

/// RED (the mirrored failure mechanism): this process opens, writes, and `chmod`s a real
/// wrapper through one descriptor, forks a sibling while that writer is still open and
/// holds it before its first `exec`, closes its own writer, and then makes exactly one
/// direct `exec` of the wrapper, which Linux must refuse with `ETXTBSY` because the held
/// sibling still owns a writable reference to that inode.
#[cfg(target_os = "linux")]
fn red_writer_inherited_and_exec_refused(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    let wrapper = dir.join("cli-wrapper.sh");
    let mut writer = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(&wrapper)
        .unwrap_or_else(|error| panic!("open {}: {error}", wrapper.display()));
    // The exact wrapper body `write_spawn_wrapper` writes, written and chmodded through
    // this open descriptor, so a held writable-open reference is the only reason an `exec`
    // of it can fail.
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$$\" > '{}'\nexec '{}' \"$@\"\n",
        dir.join("child.pid").display(),
        "/bin/sleep"
    );
    writer
        .write_all(script.as_bytes())
        .unwrap_or_else(|error| panic!("write {}: {error}", wrapper.display()));
    // Safety: our own open descriptor.
    assert_eq!(
        unsafe { libc::fchmod(writer.as_raw_fd(), 0o755) },
        0,
        "fchmod {}: {}",
        wrapper.display(),
        std::io::Error::last_os_error()
    );
    let mut sibling = HeldSibling::hold(writer.as_raw_fd(), "the red held sibling");
    let inherited = sibling
        .inherited
        .expect("the red sibling reported no inherited writer");
    assert_eq!(
        inherited,
        Inherited::of(sibling.pid, writer.as_raw_fd()),
        "the red sibling did not inherit the writer"
    );
    assert!(inherited.writable(), "the inherited writer is not writable");
    assert!(
        inherited.cloexec(),
        "the inherited writer is not close-on-exec, so this is not the mirrored window"
    );
    report_identity(
        "red",
        "writer inherited by the sibling held before its first exec",
        &inherited,
    );
    // Close this process's writer: only the held sibling's inherited copy survives.
    drop(writer);
    assert!(
        holds_descriptor(
            inherited.pid,
            inherited.dev,
            inherited.ino,
            "the red sibling"
        ),
        "/proc no longer showed the held sibling's writer after the parent closed it"
    );
    let refused = Command::new(&wrapper)
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let error = refused
        .err()
        .expect("the direct wrapper exec succeeded while a sibling held its writer");
    assert_eq!(
        error.raw_os_error(),
        Some(libc::ETXTBSY),
        "the one direct exec failed with the wrong errno: {error}"
    );
    report_refusal("red", &error);
    sibling.release_and_reap("the red held sibling");
}

/// GREEN (the repaired topology): this process holds a comparable sibling — forked while a
/// writable descriptor of a different file was open — while the real `tests_inject`
/// `fault_sequence_helper` runs the whole restart-after-round-2 sequence in its own
/// process, the only process that ever opens that wrapper's writer. The helper's direct
/// exec, pid registration, boundary hold, kill, and reap all succeed while the sibling
/// stays held, and `/proc` shows the held sibling's descriptors include the stand-in
/// writer but not the wrapper the helper wrote.
#[cfg(target_os = "linux")]
fn green_repaired_sequence_succeeds_while_sibling_held(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    let stand_in = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(dir.join("stand-in"))
        .unwrap_or_else(|error| panic!("open {}: {error}", dir.join("stand-in").display()));
    let mut sibling = HeldSibling::hold(stand_in.as_raw_fd(), "the green held sibling");
    let inherited = sibling
        .inherited
        .expect("the green sibling reported no inherited descriptor");
    assert!(
        inherited.writable(),
        "the green sibling's stand-in writer is not writable"
    );
    report_identity(
        "green",
        "comparable writer inherited by the sibling held before its first exec",
        &inherited,
    );
    let helper_dir = dir.join("helper");
    let run = run_repaired_helper_sequence(&helper_dir);
    assert_repaired_helper_verdict(&run);
    assert_held_sibling_isolated(&inherited, &helper_dir);
    report_identity(
        "green",
        "repaired sequence succeeded while a comparable sibling was held",
        &inherited,
    );
    sibling.release_and_reap("the green held sibling");
}

/// Spawn the real `tests_inject` `fault_sequence_helper` as its own process, on the real
/// restart-after-round-2 arm with a fresh helper directory, and supervise it to its
/// verdict. That helper process is the only process that ever opens this wrapper's
/// writer, which is the whole repair this witness is mirroring.
#[cfg(target_os = "linux")]
fn run_repaired_helper_sequence(helper_dir: &Path) -> HelperRun {
    fs::create_dir_all(helper_dir).unwrap();
    let helper = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "context_eval::tests_inject::fault_sequence_helper",
            "--nocapture",
        ])
        .env(FAULT_SEQUENCE_HELPER, faults::RESTART_AFTER_ROUND_2)
        .env(FAULT_SEQUENCE_DIR, helper_dir.display().to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn the repaired fault-sequence helper");
    let run = supervise_helper(helper);
    println!(
        "#264 green: repaired helper output while the sibling was held:\n{}",
        run.output
    );
    run
}

/// Judge the repaired helper's own supervised run: inside its bound, supervised without
/// failure, reaped with a successful exit, and reporting this arm's verdict line.
#[cfg(target_os = "linux")]
fn assert_repaired_helper_verdict(run: &HelperRun) {
    assert!(
        !run.timed_out,
        "the repaired helper outlived its bound while the sibling was held:\n{}",
        run.output
    );
    assert!(
        run.failure.is_none(),
        "supervising the repaired helper failed: {}\n{}",
        run.failure.unwrap_or_default(),
        run.output
    );
    assert!(
        run.status.expect("supervise always reaps").success(),
        "the repaired sequence failed while a held sibling never saw its writer:\n{}",
        run.output
    );
    let verdict = format!("arm-sequence-ok:{}", faults::RESTART_AFTER_ROUND_2);
    assert!(
        run.output.lines().any(|line| line.trim() == verdict),
        "the repaired helper never reported `{verdict}`:\n{}",
        run.output
    );
}

/// Check the held sibling's real descriptor set after the repaired sequence: it still
/// holds the stand-in writer it was forked with, and it never held the wrapper the helper
/// wrote and exec'd, which is the isolation the repaired topology exists to give.
#[cfg(target_os = "linux")]
fn assert_held_sibling_isolated(inherited: &Inherited, helper_dir: &Path) {
    assert!(
        holds_descriptor(
            inherited.pid,
            inherited.dev,
            inherited.ino,
            "the green sibling"
        ),
        "the green sibling lost the stand-in writer it was held with"
    );
    let wrapper = fs::metadata(helper_dir.join("cli-wrapper.sh")).unwrap_or_else(|error| {
        panic!(
            "stat {}: {error}",
            helper_dir.join("cli-wrapper.sh").display()
        )
    });
    assert!(
        !holds_descriptor(
            inherited.pid,
            wrapper.dev(),
            wrapper.ino(),
            "the green sibling"
        ),
        "the held sibling held the helper's wrapper writer"
    );
}

/// Linux-only entry: exec the witness in a process of its own and check its verdict, so
/// neither intentional held fork ever happens in the library test process.
#[test]
#[cfg(target_os = "linux")]
fn held_sibling_inherits_wrapper_writer_and_repairs_isolate_it() {
    let helper = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "context_eval::tests_etxtbsy::witness_helper",
            "--nocapture",
        ])
        .env(WITNESS_HELPER, "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn the #264 witness helper");
    let run = supervise_helper(helper);
    println!("#264 witness helper output:\n{}", run.output);
    assert!(
        !run.timed_out,
        "the #264 witness helper outlived its bound; output so far:\n{}",
        run.output
    );
    assert!(
        run.failure.is_none(),
        "supervising the #264 witness helper failed: {}\n{}",
        run.failure.unwrap_or_default(),
        run.output
    );
    assert!(
        run.status.expect("supervise always reaps").success(),
        "the #264 witness helper failed:\n{}",
        run.output
    );
    assert!(
        run.output
            .lines()
            .any(|line| line.trim() == "etxtbsy-witness-ok"),
        "the #264 witness helper never reported its verdict:\n{}",
        run.output
    );
}

/// The witness process itself: the red half, then the green half, then one verdict line.
#[test]
#[cfg(target_os = "linux")]
fn witness_helper() {
    if std::env::var_os(WITNESS_HELPER).is_none() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("ctxeval-etxtbsy-{}", crate::harness::uniq()));
    witness_header();
    red_writer_inherited_and_exec_refused(&dir.join("red"));
    green_repaired_sequence_succeeds_while_sibling_held(&dir.join("green"));
    let _ = fs::remove_dir_all(&dir);
    println!("etxtbsy-witness-ok");
}
