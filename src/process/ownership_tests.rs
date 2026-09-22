use super::*;

// Retain the Child handle (and therefore its unreaped identity) until cleanup.
struct TestChild(std::process::Child);
impl std::ops::Deref for TestChild {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for TestChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl Drop for TestChild {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(Some(_))) {
            return;
        }
        let _ = self.0.kill();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !matches!(self.0.try_wait(), Ok(Some(_))) {
            if Instant::now() >= deadline {
                eprintln!("test child {} did not terminate after SIGKILL", self.0.id());
                std::process::abort();
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn native_spec(args: Vec<String>, env: Vec<(String, String)>, timeout: Duration) -> CmdSpec {
    CmdSpec {
        program: std::env::current_exe().unwrap().to_str().unwrap().into(),
        args,
        cwd: None,
        cwd_fd: None,
        env_add: env,
        timeout,
        max_output: 4096,
    }
}

// Executed in a fresh native test runner, never forked Rust test state.
#[test]
fn nested_native_worker() {
    let Ok(path) = std::env::var("OWNERSHIP_PIDFILE") else {
        return;
    };
    install_cancellation_signal_handlers().unwrap();
    std::fs::write(format!("{path}.native"), std::process::id().to_string()).unwrap();
    let outcome = run_sh(
        &format!("echo $$ > '{path}.tmp'; mv '{path}.tmp' '{path}'; exec sleep 120"),
        None,
        Duration::from_secs(110),
        4096,
        vec![],
    )
    .unwrap();
    assert!(!outcome.timed_out);
}

#[test]
fn nested_native_cleanup() {
    let _serial = active_run_lock();
    // The outer native process runs run_cmd; its shell backgrounds another native
    // runtime, which creates a second managed scope. Old unconditional setsid
    // orphaned the innermost sleep when the outer command completed/cancelled.
    let mut peer = TestChild(Command::new("sleep").arg("120").spawn().unwrap());
    for mode in [
        "success",
        "failure",
        "timeout",
        "cancellation",
        "owner_sigkill",
        "assertion_failure",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("leaf.pid");
        let spec = native_spec(
            vec![
                "--exact".into(),
                "process::ownership_tests::outer_native_worker".into(),
                "--nocapture".into(),
            ],
            vec![
                ("OWNERSHIP_PIDFILE".into(), pidfile.display().to_string()),
                ("OWNERSHIP_MODE".into(), mode.into()),
            ],
            Duration::from_secs(10),
        );
        let mut command = Command::new(&spec.program);
        command
            .args(&spec.args)
            .envs(spec.env_add)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut worker = TestChild(command.spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(10);
        let pid = await_leaf(&mut worker, &pidfile, deadline, mode);
        if mode == "assertion_failure" {
            let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _owned_worker = worker;
                panic!("injected failure with a live nested native tree");
            }));
            assert!(failure.is_err());
        } else {
            if mode == "cancellation" || mode == "owner_sigkill" {
                unsafe {
                    libc::kill(
                        worker.id() as i32,
                        if mode == "owner_sigkill" {
                            libc::SIGKILL
                        } else {
                            libc::SIGTERM
                        },
                    );
                }
            }
            while worker.try_wait().unwrap().is_none() {
                assert!(Instant::now() < deadline, "{mode}: worker stuck");
                std::thread::sleep(Duration::from_millis(10));
            }
            if mode != "cancellation" && mode != "owner_sigkill" {
                assert!(
                    worker.wait().unwrap().success(),
                    "{mode}: native worker failed"
                );
            }
        }
        let native_pid: i32 = std::fs::read_to_string(format!("{}.native", pidfile.display()))
            .unwrap()
            .parse()
            .unwrap();
        assert_descendants_gone([pid, native_pid], deadline, mode);
    }
    assert!(
        peer.try_wait().unwrap().is_none(),
        "unowned peer was killed"
    );
    peer.kill().unwrap();
    peer.wait().unwrap();
}

#[test]
fn outer_native_worker() {
    let Ok(mode) = std::env::var("OWNERSHIP_MODE") else {
        return;
    };
    install_cancellation_signal_handlers().unwrap();
    let path = std::env::var("OWNERSHIP_PIDFILE").unwrap();
    let exe = std::env::current_exe().unwrap();
    let tail = match mode.as_str() {
        "success" => "exit 0",
        "failure" => "exit 7",
        _ => "sleep 120",
    };
    let script = format!("OWNERSHIP_PIDFILE='{}' '{}' --exact process::ownership_tests::nested_native_worker > /dev/null 2>&1 & while [ ! -s '{}' ]; do sleep 0.01; done; {}", path, exe.display(), path, tail);
    let outcome = run_sh(
        &script,
        None,
        if mode == "timeout" {
            Duration::from_secs(2)
        } else {
            Duration::from_secs(30)
        },
        4096,
        vec![],
    )
    .unwrap();
    assert_eq!(outcome.timed_out, mode == "timeout");
    if mode == "success" {
        assert_eq!(outcome.status, Some(0));
    }
    if mode == "failure" {
        assert_eq!(outcome.status, Some(7));
    }
}

#[test]
fn raw_child_guard_cleans_up_on_panic() {
    let mut pid = 0;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let child = TestChild(Command::new("sleep").arg("120").spawn().unwrap());
        pid = child.id() as i32;
        panic!("injected assertion failure after spawn");
    }));
    assert!(result.is_err());
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "guard left child alive");
}

#[test]
fn closed_stdio_worker() {
    if std::env::var_os("OWNERSHIP_CLOSED_STDIO").is_none() {
        return;
    }
    unsafe {
        libc::close(0);
        libc::close(1);
        libc::close(2);
    }
    let output = run_sh(
        "printf scope-output",
        None,
        Duration::from_secs(5),
        4096,
        vec![],
    )
    .unwrap();
    assert_eq!(output.status, Some(0));
    assert_eq!(output.stdout, b"scope-output");
}

#[test]
fn ownership_launch_with_closed_stdio() {
    let mut child = TestChild(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "process::ownership_tests::closed_stdio_worker",
                "--nocapture",
            ])
            .env("OWNERSHIP_CLOSED_STDIO", "1")
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "closed stdio launch failed: {status}");
            break;
        }
        assert!(Instant::now() < deadline, "closed stdio worker stuck");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn assert_descendants_gone(pids: [i32; 2], deadline: Instant, mode: &str) {
    for pid in pids {
        loop {
            let alive = unsafe { libc::kill(pid, 0) } == 0;
            if !alive {
                break;
            }
            // Linux init may leave an orphan zombie briefly; it cannot run or own FDs.
            #[cfg(target_os = "linux")]
            if std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| {
                s.split(')')
                    .nth(1)
                    .is_some_and(|s| s.trim_start().starts_with('Z'))
            }) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "{mode}: owned leaf {pid} survived"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn await_leaf(
    worker: &mut TestChild,
    pidfile: &std::path::Path,
    deadline: Instant,
    mode: &str,
) -> i32 {
    // File creation precedes writing its PID. Wait for a complete witness,
    // not merely a directory entry, especially under all-target test load.
    let pid: i32 = loop {
        if let Ok(contents) = std::fs::read_to_string(pidfile) {
            if let Ok(pid) = contents.trim().parse::<i32>() {
                assert!(pid > 0);
                break pid;
            }
        }
        assert!(
            worker.try_wait().unwrap().is_none(),
            "{mode}: worker exited before readiness"
        );
        assert!(Instant::now() < deadline, "{mode}: leaf not ready");
        std::thread::sleep(Duration::from_millis(10));
    };

    pid
}
