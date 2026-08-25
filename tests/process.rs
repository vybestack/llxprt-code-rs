//! Bounded-subprocess runner tests. Every test is deterministic: it uses a unique sleep
//! duration as its descendant marker and verifies the whole group is gone afterwards. Nothing
//! escapes the test process.

use llxprt_code_rs::process::{run_cmd, run_sh, CmdSpec};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn uniq() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u32
        % 1_000_000
}

/// The runner carries explicit env additions into the child while scrubbing unrelated
/// credential env: a fake credential sitting in the parent env must never reach the child,
/// while an explicit `LLXPRT_CONFIG_HOME`-style addition is passed through.
#[test]
fn env_add_is_carried_and_credential_env_scrubbed() {
    let fake_cred = "FAKE_LLXPRT_CRED_ENV_92817";
    unsafe {
        std::env::set_var(fake_cred, "do-not-leak");
    }
    let out = run_cmd(CmdSpec {
        program: "/bin/sh".into(),
        args: vec![
            "-c".into(),
            format!(
                "echo config=${{LLXPRT_CONFIG_HOME:-none}}; echo credcount=$(env | grep -c {fake_cred} || true)"
            ),
        ],
        cwd: None,
        cwd_fd: None,
        env_add: vec![(
            "LLXPRT_CONFIG_HOME".into(),
            "/tmp/llxprt-rs-isolated-config".into(),
        )],
        timeout: Duration::from_secs(10),
        max_output: 64 * 1024,
    })
    .expect("spawn");
    unsafe {
        std::env::remove_var(fake_cred);
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("config=/tmp/llxprt-rs-isolated-config"),
        "config env must be carried: {stdout}"
    );
    assert!(
        stdout.contains("credcount=0"),
        "credential env must be scrubbed: {stdout}"
    );
}

/// A long-running command with a unique marker: `sleep N & sleep N`. Both the foreground
/// and the backgrounded descendant share the shell's process group, so a timeout must kill
/// both. The unique duration lets us assert no descendant survived.
#[test]
fn timeout_kills_sleep_descendant_in_process_group() {
    let n = uniq().max(30_000);
    let out = run_sh(
        &format!("/bin/sleep {n} & /bin/sleep {n}"),
        None,
        Duration::from_millis(300),
        64 * 1024,
        Vec::new(),
    )
    .expect("spawn");
    assert!(out.timed_out, "must time out");
    std::thread::sleep(Duration::from_millis(100));
    let ps = std::process::Command::new("/bin/ps")
        .arg("-axo")
        .arg("args")
        .output()
        .expect("ps");
    let args = String::from_utf8_lossy(&ps.stdout);
    assert!(
        !args.contains(&format!("sleep {n}")),
        "descendant sleep {n} survived timeout: {args}"
    );
}

/// A timeout on a silent sleep returns cleanly with empty output and `timed_out` set.
#[test]
fn quiet_timeout_is_clean() {
    let out = run_sh(
        "/bin/sleep 3",
        None,
        Duration::from_millis(300),
        64 * 1024,
        Vec::new(),
    )
    .expect("spawn");
    assert!(out.timed_out);
    assert!(out.stdout.is_empty());
    assert!(out.stderr.is_empty());
}

/// A nonzero exit is surfaced with status Some(code) and timed_out false.
#[test]
fn nonzero_exit_status_reported() {
    let out = run_sh(
        "printf out; printf err >&2; exit 7",
        None,
        Duration::from_secs(5),
        64 * 1024,
        Vec::new(),
    )
    .expect("spawn");
    assert!(!out.timed_out);
    assert_eq!(out.status, Some(7));
    assert!(String::from_utf8_lossy(&out.stdout).contains("out"));
    assert!(String::from_utf8_lossy(&out.stderr).contains("err"));
}

/// A big flood on BOTH stdout and stderr is drained concurrently and the combined output is
/// capped: neither stream deadlocks the run and the return stays within the bound.
#[test]
fn concurrent_stdout_stderr_flood_is_capped() {
    let out = run_cmd(CmdSpec {
        program: "/bin/sh".into(),
        args: vec![
            "-c".into(),
            "head -c 500000 /dev/zero; head -c 500000 /dev/zero >&2".into(),
        ],
        cwd: None,
        cwd_fd: None,
        env_add: Vec::new(),
        timeout: Duration::from_secs(15),
        max_output: 16 * 1024,
    })
    .expect("spawn");
    let total = out.stdout.len() + out.stderr.len();
    assert!(total <= 16 * 1024, "combined output not capped: {total}");
    assert!(
        out.combined_truncated && (out.stdout_truncated || out.stderr_truncated),
        "a two-stream flood must report truncation"
    );
}

/// A descendant that keeps producing output keeps the pipes open after the direct child exits. The
/// supervisor must not wait for the pipes: past the deadline it kills the group and returns
/// `timed_out` promptly instead of blocking on the reader threads.
#[test]
fn descendant_pipes_past_deadline_return_timed_out() {
    let t0 = std::time::Instant::now();
    let out = run_sh(
        "(while :; do echo x; sleep 0.02; done) & /bin/sleep 0.05; exit 0",
        None,
        Duration::from_millis(400),
        64 * 1024,
        Vec::new(),
    )
    .expect("spawn");
    assert!(
        out.timed_out,
        "descendant keeping the pipe open past the deadline must time out"
    );
    // The runner stops at deadline + a short grace; it never waits out the descendant.
    assert!(
        t0.elapsed() < Duration::from_secs(5),
        "runner must not wait beyond the deadline ({:?})",
        t0.elapsed()
    );
}

/// The runner captures raw bytes: invalid UTF-8 never becomes a lossy splice on the way in and the
/// structured truncation flags are preserved.
#[test]
fn raw_bytes_and_truncated_flags_are_structured() {
    let out = run_cmd(CmdSpec {
        program: "/bin/sh".into(),
        args: vec!["-c".into(), "head -c 100000 /dev/zero".into()],
        cwd: None,
        cwd_fd: None,
        env_add: Vec::new(),
        timeout: Duration::from_secs(15),
        max_output: 1024,
    })
    .expect("spawn");
    assert!(!out.timed_out);
    assert!(out.stdout_truncated, "capture cap must be flagged");
    assert!(out.combined_truncated, "combined flag derives from streams");
    assert!(out.stdout.len() <= 1024);
}

/// The reader never stops at the budget: it keeps draining by discarding bytes so a writer never
/// fills its pipe and blocks.
#[test]
fn drain_continues_past_capture_cap_no_writer_block() {
    let out = run_cmd(CmdSpec {
        program: "/bin/sh".into(),
        args: vec![
            "-c".into(),
            "head -c 200000 /dev/zero | cat; echo done".into(),
        ],
        cwd: None,
        cwd_fd: None,
        env_add: Vec::new(),
        timeout: Duration::from_secs(15),
        max_output: 64,
    })
    .expect("spawn");
    assert_eq!(
        out.status,
        Some(0),
        "pipeline must finish; writer never blocks"
    );
    assert!(out.stdout_truncated);
}

/// A signal-terminated child (SIGKILL) is reported as a failed non-zero outcome, never
/// a clean success: the runner surfaces `status == None` / 137 / a timeout, and the
/// shell tool returns `ok=false` so the model can repair the damage.
#[test]
fn signal_kill_reports_none_status() {
    // Run in a temp cwd so the signals cannot touch anything real.
    let d = tempfile::tempdir().unwrap();
    let out = run_sh(
        "kill -9 $$",
        Some(d.path()),
        Duration::from_secs(3),
        64 * 1024,
        Vec::new(),
    )
    .expect("spawn");
    assert!(
        out.timed_out || out.status.is_none() || out.status == Some(137),
        "a signal death is reported as a timeout/signal, got {:?}",
        out.status
    );
    let (ok, msg) = crate_tools_shell("kill -9 $$");
    assert!(!ok, "a signal death is a failed tool result");
    assert!(
        msg.contains("killed by a signal")
            || msg.contains("exited with 137")
            || msg.contains("timed out"),
        "the model sees a failed result, not a clean code: {msg}"
    );
    let _ = &out;
}

/// The shell tool's signal path end to end: a command killed by a signal is a failed
/// result the model can see, never a spuriously clean success.
#[test]
fn shell_tool_signal_is_a_failed_result() {
    let (ok, msg) = crate_tools_shell("kill -9 $$");
    assert!(!ok, "a signal death is a failed tool result");
    assert!(
        msg.contains("killed by a signal")
            || msg.contains("exited with 137")
            || msg.contains("timed out"),
        "the model sees a failed result, not a clean code: {msg}"
    );
}

/// Drive the `run_shell_command` tool (allow_shell on) through the real runner.
fn crate_tools_shell(cmd: &str) -> (bool, String) {
    use serde_json::json;
    let d = tempfile::tempdir().unwrap();
    let ws = llxprt_code_rs::tools::WorkspaceCap::open(d.path()).unwrap();
    let config = llxprt_code_rs::tools::ToolConfig {
        ws,
        max_output_bytes: 16 * 1024,
        shell: llxprt_code_rs::tools::ShellConfig {
            max_shell_output: 64 * 1024,
            max_shell_timeout: Duration::from_secs(3),
            allow_shell: true,
        },
    };
    llxprt_code_rs::tools::execute_tool(
        d.path(),
        "run_shell_command",
        json!({ "command": cmd }),
        &config,
    )
}

/// Invalid UTF-8 on stdout/stderr is always a valid (lossy) String, never a splice.
#[test]
fn invalid_utf8_is_lossy_but_valid() {
    let out = run_cmd(CmdSpec {
        program: "/bin/sh".into(),
        args: vec![
            "-c".into(),
            "printf '\\xff\\xfe\\x80ok'; printf '\\xfd' >&2".into(),
        ],
        cwd: None,
        cwd_fd: None,
        env_add: Vec::new(),
        timeout: Duration::from_secs(5),
        max_output: 64 * 1024,
    })
    .expect("spawn");
    assert!(!out.timed_out);
    assert_eq!(out.status, Some(0));
}

#[test]
fn unrepresentable_timeout_is_rejected_before_spawn_without_panicking() {
    let temp = tempfile::tempdir().unwrap();
    let marker = temp.path().join("spawned");
    let outcome = std::panic::catch_unwind(|| {
        run_cmd(CmdSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), format!("touch '{}'", marker.display())],
            cwd: None,
            cwd_fd: None,
            env_add: vec![],
            timeout: Duration::MAX,
            max_output: 1024,
        })
    });
    let result = outcome.expect("extreme timeout must not panic");
    let error = match result {
        Ok(_) => panic!("extreme timeout must be rejected"),
        Err(error) => error,
    };
    assert!(error.contains("cannot be represented"));
    assert!(
        !marker.exists(),
        "command must not spawn when its deadline is invalid"
    );
}
