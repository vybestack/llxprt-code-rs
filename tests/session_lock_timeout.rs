#![cfg(unix)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn cross_process_session_lock_times_out_without_mutating_slots() {
    let temp = tempfile::tempdir().unwrap();
    let profiles = temp.path().join("profiles");
    let session_dir = temp.path().join("code-rs-sessions/lock-contention");
    std::fs::create_dir_all(&profiles).unwrap();
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(
        profiles.join("lock.json"),
        serde_json::json!({
            "provider": "openai",
            "model": "lock-test",
            "ephemeralSettings": {
                "base-url": "http://127.0.0.1:9",
                "auth-key": "lock-test-key"
            }
        })
        .to_string(),
    )
    .unwrap();

    let lock_path = session_dir.join(".lock");
    let ready = temp.path().join("holder-ready");
    let script = "import fcntl, pathlib, sys, time\n\
                  f = open(sys.argv[1], 'a+b')\n\
                  fcntl.flock(f.fileno(), fcntl.LOCK_EX)\n\
                  pathlib.Path(sys.argv[2]).write_text('ready')\n\
                  time.sleep(30)\n";
    let mut holder = Command::new("python3")
        .arg("-c")
        .arg(script)
        .arg(&lock_path)
        .arg(&ready)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let ready_deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() && Instant::now() < ready_deadline {
        assert!(
            holder.try_wait().unwrap().is_none(),
            "lock holder exited early"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(ready.exists(), "lock holder did not acquire the lock");

    let started = Instant::now();
    let mut command = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"));
    command
        .env("LLXPRT_CONFIG_HOME", temp.path())
        .arg("--profile")
        .arg("lock")
        .arg("--session")
        .arg("lock-contention")
        .arg("--cwd")
        .arg(temp.path())
        .arg("-p")
        .arg("must stop at the session lock")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut cli = command.spawn().unwrap();
    let cli_deadline = Instant::now() + Duration::from_secs(12);
    loop {
        if cli.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= cli_deadline {
            cli.kill().unwrap();
            cli.wait().unwrap();
            holder.kill().unwrap();
            holder.wait().unwrap();
            panic!("CLI did not honor the bounded session-lock deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = cli.wait_with_output().unwrap();
    holder.kill().unwrap();
    holder.wait().unwrap();
    assert!(!output.status.success());
    assert!(started.elapsed() >= Duration::from_secs(5));
    assert!(started.elapsed() < Duration::from_secs(10));
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["status"], "error");
    assert!(response.to_string().contains("session lock timed out"));
    assert!(!session_dir.join("session.json").exists());
    assert!(!session_dir.join("session.alt.json").exists());
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path)
        .unwrap();
    fs2::FileExt::try_lock_exclusive(&file).unwrap();
    fs2::FileExt::unlock(&file).unwrap();
}
