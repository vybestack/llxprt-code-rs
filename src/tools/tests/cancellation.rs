//! Cancellation-lifecycle tests split out of the main tools test module so both
//! files stay under the quality gate's 800 effective-LOC ceiling.
use std::time::Duration;

const CANCELLATION_CHILD: &str = "LLXPRT_TEST_CANCELLATION_CHILD";
const LAUNCH_READY: &str = "LLXPRT_TEST_LAUNCH_READY";
const LAUNCH_SIDE_EFFECT: &str = "LLXPRT_TEST_LAUNCH_SIDE_EFFECT";

fn spawn_cancellation_child() -> std::process::Child {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "tools::tests::cancellation::cancelling_the_worker_subprocess_helper",
            "--nocapture",
        ])
        .env(CANCELLATION_CHILD, "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap()
}

fn spawn_launch_cancellation_child(
    marker: &std::path::Path,
    side_effect: &std::path::Path,
) -> std::process::Child {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "tools::tests::cancellation::cancelling_during_launch_subprocess_helper",
            "--nocapture",
        ])
        .env(LAUNCH_READY, marker)
        .env(LAUNCH_SIDE_EFFECT, side_effect)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap()
}

/// Read the `"<pgid> [<starttime>]"` line the helper prints once its `run_cmd` registers the
/// group. The starttime (present on Linux) anchors the cohort identity for the liveness probe.
fn read_reported_pgid(child: &mut std::process::Child) -> (libc::pid_t, Option<u64>) {
    use std::io::BufRead;
    let stdout = child.stdout.take().unwrap();
    for line in std::io::BufReader::new(stdout).lines() {
        let line = line.unwrap();
        // split_whitespace already skips leading/trailing blanks, so no trim() first.
        let mut parts = line.split_whitespace();
        if let Some(Ok(pgid)) = parts.next().map(str::parse::<i32>) {
            assert!(pgid > 0, "helper reported a non-group pgid: {pgid}");
            // The helper prints 0 when it could not read the leader's stat; treat that as
            // "cohort unknown" (bare pgrp match) rather than a window no member can satisfy.
            let starttime = parts
                .next()
                .and_then(|token| token.parse::<u64>().ok())
                .filter(|start| *start > 0);
            return (pgid, starttime);
        }
    }
    panic!("helper exited before reporting its tool pgid");
}

/// One live (non-zombie) `/proc` member of the group, with the `starttime` field that
/// distinguishes processes spawned in the supervised cohort from pid-recycled strangers.
#[cfg(target_os = "linux")]
#[derive(Debug)]
struct LiveMember {
    pid: i32,
    state: char,
    comm: String,
    starttime: u64,
}

#[cfg(target_os = "linux")]
impl std::fmt::Display for LiveMember {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {} {} (start {})",
            self.pid, self.state, self.comm, self.starttime
        )
    }
}

/// Parse `/proc/<pid>/stat` down to `(state, pgrp, starttime, comm)`. `None` when the file is
/// unreadable (process exited or non-Linux path).
#[cfg(target_os = "linux")]
fn proc_stat(pid: libc::pid_t) -> Option<(u8, libc::pid_t, u64, String)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The comm field can contain spaces and parens — the classic /proc stat parsing trap — so
    // parse AFTER the last ')', which closes comm. Tokens after it are, in order: state(f3),
    // ppid(f4), pgrp(f5), ... starttime(f22, token index 19).
    let after_comm = stat.rsplit_once(')')?;
    let comm = after_comm
        .0
        .split_once('(')
        .map_or("?".to_string(), |(_, c)| c.to_string());
    let mut fields = after_comm.1.split_whitespace();
    let state = fields.next()?.as_bytes().first().copied()?;
    fields.next(); // ppid
    let pgrp: libc::pid_t = fields.next()?.parse().ok()?;
    let starttime: u64 = fields.nth(16)?.parse().ok()?;
    Some((state, pgrp, starttime, comm))
}

/// `starttime` of the group leader — the moment the supervised cohort was born. Registered
/// before the kill, so any process with a later `starttime` cannot be a cohort remnant.
#[cfg(target_os = "linux")]
fn leader_starttime(pgid: libc::pid_t) -> Option<u64> {
    proc_stat(pgid).map(|(_, _, starttime, _)| starttime)
}

/// Every LIVE (non-zombie) `/proc` member whose pgrp matches `pgid`. `None` when `/proc` is
/// unreadable.
#[cfg(target_os = "linux")]
fn live_group_snapshot(pgid: libc::pid_t) -> Option<Vec<LiveMember>> {
    let entries = std::fs::read_dir("/proc").ok()?;
    let mut members = Vec::new();
    for entry in entries.flatten() {
        let Ok(pid) = entry
            .file_name()
            .to_str()
            .unwrap_or_default()
            .parse::<i32>()
        else {
            continue; // not a pid directory: self, thread-self, acpi, ...
        };
        let Some((state, pgrp, starttime, comm)) = proc_stat(pid) else {
            continue; // process exited between readdir and read
        };
        if pgrp == pgid && state != b'Z' {
            members.push(LiveMember {
                pid,
                state: state as char,
                comm,
                starttime,
            });
        }
    }
    Some(members)
}

/// True when the group still has a LIVE member from the supervised cohort. The kill frees the
/// leader's pid; under CI fork churn that pid can be reissued to an unrelated `setsid` shell
/// whose group then numerically equals ours, so a bare pgrp match is not identity. A member
/// counts only when its `starttime` falls within the cohort window: no later than the leader's
/// own birth plus one second of fork slack (the `sleep` child is forked milliseconds after the
/// leader execs). `cohort == None` (leader stat unreadable) keeps the bare pgrp match so the
/// test still catches true survivors. `None` when `/proc` is unreadable; callers keep polling.
#[cfg(target_os = "linux")]
fn cohort_fork_slack_ticks() -> u64 {
    // Safety: `_SC_CLK_TCK` is a valid `sysconf` selector. Do not assume Linux uses 100 Hz.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    u64::try_from(ticks).unwrap_or(100)
}

#[cfg(target_os = "linux")]
fn group_has_live_member(pgid: libc::pid_t, cohort: Option<u64>) -> Option<bool> {
    let members = live_group_snapshot(pgid)?;
    let slack = cohort_fork_slack_ticks();
    Some(
        members
            .iter()
            .any(|m| cohort.is_none_or(|born| m.starttime <= born.saturating_add(slack))),
    )
}

/// Deliberately reach the pre-exec barrier before `run_cmd` has a `Child` to register.
#[test]
fn cancelling_during_launch_subprocess_helper() {
    if std::env::var_os(LAUNCH_READY).is_none() {
        return;
    }
    crate::process::install_cancellation_signal_handlers().unwrap();
    let side_effect = std::env::var_os(LAUNCH_SIDE_EFFECT).unwrap();
    let _ = crate::process::run_sh(
        "touch \"$LLXPRT_TEST_LAUNCH_SIDE_EFFECT\"",
        None,
        Duration::from_secs(60),
        1024,
        vec![(
            LAUNCH_SIDE_EFFECT.to_string(),
            side_effect.to_string_lossy().into_owned(),
        )],
    );
}

#[test]
fn cancelling_the_worker_subprocess_helper() {
    if std::env::var_os(CANCELLATION_CHILD).is_none() {
        return;
    }
    crate::process::install_cancellation_signal_handlers().unwrap();
    std::thread::spawn(|| {
        // `run_cmd` blocks until the command finishes, so the side thread publishes the group the
        // moment the runner registers it.
        loop {
            let pgid = crate::process::active_group();
            if pgid > 0 {
                // The leader's `starttime` travels with the pgid so the parent can later reject
                // pid-recycled stranger groups that numerically reuse the dead pgid.
                #[cfg(target_os = "linux")]
                println!("{} {}", pgid, leader_starttime(pgid).unwrap_or(0));
                #[cfg(not(target_os = "linux"))]
                println!("{pgid}");
                use std::io::Write;
                let _ = std::io::stdout().flush();
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    });
    let _ = crate::process::run_sh("sleep 30", None, Duration::from_secs(60), 1024, Vec::new());
}

/// The marker is written after `setsid`, but while the child cannot reach the launch handoff.
#[test]
fn cancelling_during_launch_kills_the_unpublished_child_before_exec() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("launch-ready");
    let side_effect = directory.path().join("tool-executed");
    let mut worker = spawn_launch_cancellation_child(&marker, &side_effect);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !marker.is_file() {
        assert!(
            std::time::Instant::now() < deadline,
            "launch barrier did not report readiness: {:?}",
            worker.try_wait().ok().flatten()
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let pgid: libc::pid_t = std::fs::read_to_string(&marker).unwrap().parse().unwrap();
    assert!(
        !side_effect.exists(),
        "tool executed before launch cancellation"
    );
    // Safety: cancellation hits the exact post-fork/pre-publication handoff.
    assert_eq!(
        unsafe { libc::kill(worker.id() as libc::pid_t, libc::SIGTERM) },
        0
    );
    assert_eq!(worker.wait().unwrap().code(), Some(143));
    #[cfg(target_os = "linux")]
    {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if matches!(proc_stat(pgid), None | Some((b'Z', _, _, _))) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "unpublished child {pgid} survived worker cancellation"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while unsafe { libc::kill(pgid, 0) } == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "unpublished child {pgid} survived worker cancellation"
            );
            std::thread::yield_now();
        }
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH),
            "launch child {pgid} probe failed unexpectedly"
        );
    }
    assert!(
        !side_effect.exists(),
        "tool executed after launch cancellation"
    );
}

/// `kill(pgid, 0)` — the v1 probe — counts orphan zombies (unreaped by non-reaping subreaper
/// ancestors on busy CI runners) and pids reused by successor processes as alive; the release
/// job's second test invocation hits both. v2 moved to a `/proc` pgrp + state scan, but a
/// REISSUED pid can lead an unrelated `setsid` group whose pgrp numerically equals the dead
/// one, so v3 pins cohort identity: a member counts as a survivor only when its `starttime`
/// places it inside the supervised cohort's birth window. Other hosts keep the legacy probe.
#[test]
fn cancelling_the_worker_kills_the_active_tool_group() {
    // Born deaf on purpose: block SIGTERM on this thread so the helper inherits the block
    // across exec — the exact CI condition this test regressed on. The production unblock in
    // `install_cancellation_signal_handlers` must punch through it, otherwise the TERM sent
    // below stays pending forever and the supervised group outlives the worker.
    let mut blocked: libc::sigset_t = unsafe { std::mem::zeroed() };
    let mut previous: libc::sigset_t = unsafe { std::mem::zeroed() };
    // Safety: `zeroed` supplies storage; the set init and mask swap touch this thread only.
    unsafe {
        libc::sigemptyset(&mut blocked);
        libc::sigaddset(&mut blocked, libc::SIGTERM);
        assert_eq!(
            libc::pthread_sigmask(libc::SIG_SETMASK, &blocked, &mut previous),
            0
        );
    }
    let mut child = spawn_cancellation_child();
    // Safety: restore this thread's mask so only the spawn carried the block.
    unsafe {
        assert_eq!(
            libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut()),
            0
        );
    }
    let (pgid, cohort) = read_reported_pgid(&mut child);
    // `cohort` anchors the Linux /proc probe; the legacy probe on other hosts never reads it.
    #[cfg(not(target_os = "linux"))]
    let _ = cohort;
    // Reporting happens only after `run_cmd` has registered its freshly spawned session, so the
    // cancellation can be delivered immediately; no timing delay is needed to make this race
    // observable.
    // Safety: signal the helper pid only, exactly like `kill -TERM` on a headless worker.
    assert_eq!(
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) },
        0
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        // Reap the leader if this test happens to own it. Its status is cleanup only: reaping
        // a leader does not prove same-group descendants are gone.
        let _ = unsafe { libc::waitpid(pgid, std::ptr::null_mut(), libc::WNOHANG) };
        #[cfg(target_os = "linux")]
        {
            // Truthful /proc verdict: scan succeeded and no live (non-zombie) member remains.
            // Some(true) keeps polling; None (/proc unreadable) keeps polling to the deadline
            // rather than declaring early victory.
            if matches!(group_has_live_member(pgid, cohort), Some(false)) {
                break;
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            // Safety: signal zero only probes whether the tool's process group still exists.
            if unsafe { libc::kill(pgid, 0) } == -1
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            {
                break;
            }
        }
        #[cfg(target_os = "linux")]
        assert!(
            std::time::Instant::now() < deadline,
            "active tool process group survived worker cancellation (cohort start {:?}, live members: {:?}, helper status: {:?})",
            cohort,
            live_group_snapshot(pgid),
            child.try_wait().ok().flatten(),
        );
        #[cfg(not(target_os = "linux"))]
        assert!(
            std::time::Instant::now() < deadline,
            "active tool process group survived worker cancellation"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(143));
}
