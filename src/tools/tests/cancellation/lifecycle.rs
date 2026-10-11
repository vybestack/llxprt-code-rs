//! Each scenario runs in a fresh verifier, so assertion unwind and signal exit
//! exercise real helper ownership rather than merely testing a guard in isolation.
use super::*;
use std::path::Path;
use std::process::{Command, Stdio};

const VERIFIER_ROOT: &str = "LLXPRT_TEST_VERIFIER_ROOT";
const VERIFIER_MODE: &str = "LLXPRT_TEST_VERIFIER_MODE";
const VERIFIER_TEST: &str = "tools::tests::cancellation::lifecycle::launch_verifier_helper";

fn read_pid(path: &Path, deadline: Instant) -> i32 {
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(pid) = text.trim().parse::<i32>() {
                assert!(pid > 0);
                return pid;
            }
        }
        assert!(Instant::now() < deadline, "missing PID witness: {path:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn assert_gone(pid: i32, deadline: Instant, mode: &str) {
    loop {
        #[cfg(target_os = "linux")]
        if matches!(proc_stat(pid), None | Some((b'Z', _, _, _))) {
            return;
        }
        #[cfg(not(target_os = "linux"))]
        if unsafe { libc::kill(pid, 0) } == -1 {
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{mode}: owned helper process {pid} survived"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn launch_helper_lifecycle_leaves_no_owned_processes() {
    let mut peer = OwnedHelper::spawn(Command::new("sleep").arg("60"));
    for mode in [
        "completion",
        "assertion_failure",
        "timeout",
        "cancellation",
        "verifier_sigkill",
    ] {
        let root = tempfile::tempdir().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut command = Command::new(std::env::current_exe().unwrap());
        // Release/extracted gates can inherit a blocked TERM. Pin that condition
        // explicitly; the verifier must arrange its own cancellation delivery.
        block_verifier_term(&mut command);
        let mut verifier = OwnedHelper::spawn(
            command
                .args(["--exact", VERIFIER_TEST, "--nocapture"])
                .env(VERIFIER_ROOT, root.path())
                .env(VERIFIER_MODE, mode)
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        );
        // The ready witness is published last, after collecting every owned ID.
        read_pid(&root.path().join("ready"), deadline);
        let worker = read_pid(&root.path().join("worker"), deadline);
        let scope = read_pid(&root.path().join("scope"), deadline);
        let child = read_pid(&root.path().join("launch-ready"), deadline);
        let tool_scope = read_pid(&root.path().join("tool-scope"), deadline);
        eprintln!("{mode}: verifier={} worker={worker} helper_group={scope} pre_exec_child={child} tool_group={tool_scope}", verifier.id());
        if mode == "cancellation" || mode == "verifier_sigkill" {
            let signal = cancellation_signal(mode);
            assert_eq!(unsafe { libc::kill(verifier.id() as i32, signal) }, 0);
        }
        let status = verifier.wait_until(deadline).unwrap();
        match mode {
            "assertion_failure" => assert_eq!(status.code(), Some(101)),
            "cancellation" | "verifier_sigkill" => {
                use std::os::unix::process::ExitStatusExt;
                assert_eq!(status.signal(), Some(cancellation_signal(mode)));
            }
            _ => assert!(status.success(), "{mode}: verifier failed: {status}"),
        }
        for pid in [worker, scope, child, tool_scope] {
            assert_gone(pid, deadline, mode);
        }
        assert!(
            !root.path().join("tool-executed").exists(),
            "{mode}: tool escaped the pre-exec gate"
        );
        assert!(
            peer.try_wait().unwrap().is_none(),
            "{mode}: unrelated peer was killed"
        );
    }
    // Peer is cleaned up by its own owner, not by any scenario under test.
    drop(peer);
}

#[test]
fn launch_verifier_helper() {
    let Some(root) = std::env::var_os(VERIFIER_ROOT) else {
        return;
    };
    configure_verifier_term();
    let root = std::path::PathBuf::from(root);
    let mode = std::env::var(VERIFIER_MODE).unwrap();
    let mut worker =
        spawn_launch_cancellation_child(&root.join("launch-ready"), &root.join("tool-executed"));
    let child = read_pid(
        &root.join("launch-ready"),
        Instant::now() + Duration::from_secs(5),
    );
    let tool_scope = unsafe { libc::getpgid(child) };
    assert!(tool_scope > 0);
    for (name, pid) in [
        ("worker", worker.id() as i32),
        ("scope", worker.scope_pgid()),
        ("tool-scope", tool_scope),
        ("ready", std::process::id() as i32),
    ] {
        std::fs::write(root.join(name), pid.to_string()).unwrap();
    }
    match mode.as_str() {
        "completion" => {
            assert_eq!(unsafe { libc::kill(worker.id() as i32, libc::SIGTERM) }, 0);
            assert_eq!(
                worker
                    .wait_until(Instant::now() + Duration::from_secs(5))
                    .unwrap()
                    .code(),
                Some(143)
            );
        }
        "assertion_failure" => {
            panic!("injected assertion failure while helper is parked before publication")
        }
        "timeout" => {
            let error = worker
                .wait_until(Instant::now() + Duration::from_millis(20))
                .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        }
        "cancellation" | "verifier_sigkill" => loop {
            std::thread::sleep(Duration::from_secs(1));
        },
        _ => panic!("unknown verifier mode: {mode}"),
    }
}

fn cancellation_signal(mode: &str) -> i32 {
    if mode == "cancellation" {
        libc::SIGTERM
    } else {
        libc::SIGKILL
    }
}

fn block_verifier_term(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    // Safety: signal-mask syscalls only, in this fixture's post-fork child.
    unsafe {
        command.pre_exec(|| {
            let mut blocked: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut blocked);
            libc::sigaddset(&mut blocked, libc::SIGTERM);
            let error = libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, std::ptr::null_mut());
            if error != 0 {
                return Err(std::io::Error::from_raw_os_error(error));
            }
            Ok(())
        });
    }
}

fn configure_verifier_term() {
    // The verifier models an initiating gate, not a runtime worker: TERM must
    // exit without unwinding. Exec retains masks and ignored dispositions, so
    // install the intended default action and unblock in this fresh process.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = libc::SIG_DFL;
        libc::sigemptyset(&mut action.sa_mask);
        assert_eq!(
            libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut()),
            0
        );
        let mut signals: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut signals);
        libc::sigaddset(&mut signals, libc::SIGTERM);
        assert_eq!(
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &signals, std::ptr::null_mut()),
            0
        );
    }
}
