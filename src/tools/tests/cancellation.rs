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
        // Non-blocking reap of the (possibly adopted) former group leader. ECHILD before
        // adoption completes, or on non-Linux, is harmless and ignored.
        let _ = unsafe { libc::waitpid(pgid, std::ptr::null_mut(), libc::WNOHANG) };
        // Safety: signal zero only probes whether the tool's process group still exists.
        if unsafe { libc::kill(pgid, 0) } == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "active tool process group survived worker cancellation"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(143));
}
