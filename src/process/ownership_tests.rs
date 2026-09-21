use super::*;

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
        // Publish readiness atomically: existence must imply a complete PID.
        &format!("echo $$ > '{path}.pending'; mv '{path}.pending' '{path}'; exec sleep 120"),
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
    let mut peer = Command::new("sleep").arg("120").spawn().unwrap();
    for mode in [
        "success",
        "failure",
        "timeout",
        "cancellation",
        "owner_sigkill",
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
        let mut worker = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !pidfile.exists() {
            assert!(Instant::now() < deadline, "{mode}: leaf not ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        let pid: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
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
        let native_pid: i32 = std::fs::read_to_string(format!("{}.native", pidfile.display()))
            .unwrap()
            .parse()
            .unwrap();
        for pid in [pid, native_pid] {
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
