//! Cancellation-lifecycle tests split out of the main tools test module so both
//! files stay under the quality gate's 800 effective-LOC ceiling.
use std::time::Duration;

const CANCELLATION_CHILD: &str = "LLXPRT_TEST_CANCELLATION_CHILD";

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

/// Read the tool pgid the helper prints once its `run_cmd` registers the group.
fn read_reported_pgid(child: &mut std::process::Child) -> libc::pid_t {
    use std::io::BufRead;
    let stdout = child.stdout.take().unwrap();
    for line in std::io::BufReader::new(stdout).lines() {
        let line = line.unwrap();
        if let Ok(pgid) = line.trim().parse::<i32>() {
            assert!(pgid > 0, "helper reported a non-group pgid: {pgid}");
            return pgid;
        }
    }
    panic!("helper exited before reporting its tool pgid");
}

/// `["pid state comm"]` for every LIVE (non-zombie) member of `pgid`, judged from `/proc` so
/// that orphan zombies left on a non-reaping subreaper and pids reused by successor processes
/// after our reap both read correctly. `None` when `/proc` is unreadable.
#[cfg(target_os = "linux")]
fn live_group_snapshot(pgid: libc::pid_t) -> Option<Vec<String>> {
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
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue; // process exited between readdir and read
        };
        // The comm field can contain spaces and parens — the classic /proc stat parsing
        // trap — so parse AFTER the last ')', which closes comm. Fields after it are, in
        // order: state, ppid, pgrp.
        let Some(after_comm) = stat.rsplit_once(')') else {
            continue;
        };
        let mut fields = after_comm.1.split_whitespace();
        let state = fields
            .next()
            .and_then(|field| field.as_bytes().first().copied());
        fields.next(); // ppid: not needed, but must be consumed to reach pgrp
        let pgrp = fields.next();
        if pgrp.and_then(|value| value.parse::<i32>().ok()) == Some(pgid) && state != Some(b'Z') {
            // comm is bracketed by the first '(' and the final ')'.
            let comm = after_comm.0.split_once('(').map_or("?", |(_, comm)| comm);
            let state = state.unwrap_or(b'?') as char;
            members.push(format!("{pid} {state} {comm}"));
        }
    }
    Some(members)
}

/// True when the process group still has a LIVE (non-zombie) member on Linux, judged from
/// `/proc` so that orphan zombies left on a non-reaping subreaper and pids reused after our
/// reap both read correctly. `None` when `/proc` is unreadable; callers keep polling then.
#[cfg(target_os = "linux")]
fn group_has_live_member(pgid: libc::pid_t) -> Option<bool> {
    live_group_snapshot(pgid).map(|members| !members.is_empty())
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

/// `kill(pgid, 0)` — the v1 probe — counts orphan zombies (unreaped by non-reaping subreaper
/// ancestors on busy CI runners) and pids reused by successor processes as alive; the release
/// job's second test invocation hits both. `/proc` pgrp + state is the truthful live-member
/// signal, so on Linux group death is judged from it; other hosts keep the legacy probe.
#[test]
fn cancelling_the_worker_kills_the_active_tool_group() {
    // On Linux, make this test process a child subreaper so that descendants orphaned by the
    // terminated helper (the `setsid` group leader, in particular) reparent to *us* instead of
    // to a CI runner agent. Runner agents are subreapers that do not promptly reap foreign
    // orphans, so the killed group leader can linger as a zombie — and `kill(pgid, 0)` reports
    // zombies as alive. Adopting the chain lets us reap it ourselves below.
    #[cfg(target_os = "linux")]
    {
        // Safety: prctl with a valid option and integer argument; see PR_SET_CHILD_SUBREAPER(2).
        let rc = unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) };
        assert_eq!(
            rc,
            0,
            "PR_SET_CHILD_SUBREAPER failed: {}",
            std::io::Error::last_os_error()
        );
    }
    let mut child = spawn_cancellation_child();
    let pgid = read_reported_pgid(&mut child);
    // Let the helper settle into its supervised wait before cancelling it.
    std::thread::sleep(Duration::from_millis(100));
    // Safety: signal the helper pid only, exactly like `kill -TERM` on a headless worker.
    assert_eq!(
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) },
        0
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        // Definitive: the kernel handed us the former leader's exit status.
        if unsafe { libc::waitpid(pgid, std::ptr::null_mut(), libc::WNOHANG) } == pgid {
            break;
        }
        #[cfg(target_os = "linux")]
        {
            // Truthful /proc verdict: scan succeeded and no live (non-zombie) member remains.
            // Some(true) keeps polling; None (/proc unreadable) keeps polling to the deadline
            // rather than declaring early victory.
            if matches!(group_has_live_member(pgid), Some(false)) {
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
            "active tool process group survived worker cancellation (live members: {:?})",
            live_group_snapshot(pgid)
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
