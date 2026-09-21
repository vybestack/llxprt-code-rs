use super::*;

#[test]
fn issue286_shell_default_and_maximum_have_distinct_runtime_effects() {
    // Process ownership serializes runs within one runtime. Measure shell deadlines
    // in a fresh test process, not behind unrelated parallel tests' run lock.
    if std::env::var_os("ISSUE286_SHELL_TIMEOUT_CHILD").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tools::tests::shell_timeout::issue286_shell_default_and_maximum_have_distinct_runtime_effects", "--nocapture"])
            .env("ISSUE286_SHELL_TIMEOUT_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let mut config = cfg(d.path());
    config.shell.allow_shell = true;
    config.shell.default_shell_timeout = Duration::from_secs(1);
    config.shell.max_shell_timeout = Duration::from_secs(3);
    let execute = |args| execute_tool(d.path(), "run_shell_command", args, &config);
    // Omitted timeout uses the default, not the maximum.
    assert!(!execute(json!({"command":"sleep 2"})).0);
    // Explicit timeout may exceed the default, up to the cap.
    assert!(execute(json!({"command":"sleep 2", "timeout_seconds":3})).0);
    // A model cannot escape the maximum by asking for a larger value.
    let started = std::time::Instant::now();
    assert!(!execute(json!({"command":"sleep 10", "timeout_seconds":30})).0);
    assert!(started.elapsed() < Duration::from_secs(8));
    for bad in [json!(0), json!(-1), json!(1.5), json!("2"), json!(null)] {
        assert!(!execute(json!({"command":"true", "timeout_seconds":bad})).0);
    }
}
