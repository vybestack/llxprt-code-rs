//! Worker-repin contract exercised through the stock CLI, with no live provider.
use llxprt_code_rs::session::{RoundRecord, SessionId, SessionStore, STORE_VERSION};
use serde_json::{json, Value};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

fn bin(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"));
    command
        .env_clear()
        .env("HOME", root)
        .env("LLXPRT_CONFIG_HOME", root)
        .current_dir(root);
    command
}

fn bounded_output(command: &mut Command) -> Output {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() >= Duration::from_secs(10) {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!("capability probe exceeded 10s: {output:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.stdout.len() <= 64 * 1024);
    output
}

fn assert_probe(output: Output) {
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value, json!({"session_state_versions": [STORE_VERSION]}));
    assert_eq!(output.stdout, format!("{value}\n").as_bytes());
}

#[test]
fn probe_schema_and_closed_stdin_without_config_or_profile() {
    use std::os::unix::process::CommandExt;
    let root = tempfile::tempdir().unwrap();
    // A regular-file config home cannot be traversed or initialized. Neither profile
    // resolution nor session initialization is necessary to answer this query.
    let blocked = root.path().join("not-a-directory");
    std::fs::write(&blocked, b"untouched").unwrap();
    let mut command = bin(root.path());
    command
        .arg("--session-state-versions")
        .env("LLXPRT_CONFIG_HOME", &blocked);
    unsafe {
        command.pre_exec(|| {
            libc::close(0);
            Ok(())
        });
    }
    assert_probe(bounded_output(&mut command));
    assert_eq!(std::fs::read(&blocked).unwrap(), b"untouched");
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}

#[test]
fn probe_does_not_read_open_stdin_or_touch_existing_sessions_or_network() {
    let root = tempfile::tempdir().unwrap();
    seed(root.path());
    let before = tree(root.path());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut command = bin(root.path());
    command
        .arg("--session-state-versions")
        .stdin(Stdio::piped())
        .env(
            "HTTP_PROXY",
            format!("http://{}", listener.local_addr().unwrap()),
        )
        .env(
            "HTTPS_PROXY",
            format!("http://{}", listener.local_addr().unwrap()),
        );
    // bounded_output retains the child's unwritten stdin until exit: reading it blocks.
    assert_probe(bounded_output(&mut command));
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert_eq!(tree(root.path()), before);
}

#[test]
fn probe_rejects_other_arguments_instead_of_running_a_turn() {
    let root = tempfile::tempdir().unwrap();
    for extra in [
        vec!["--session", "s"],
        vec!["-p", "prompt"],
        vec!["--print-config"],
        vec!["--session-state-versions"],
    ] {
        let output = bin(root.path())
            .arg("--session-state-versions")
            .args(extra)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["status"], "error");
        assert!(value.get("session_state_versions").is_none());
    }
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

fn seed(root: &Path) {
    std::fs::create_dir_all(root.join("profiles")).unwrap();
    std::fs::write(root.join("profiles/fixture.json"), json!({
        "provider":"openai", "model":"fixture-model",
        "ephemeralSettings":{"base-url":"http://127.0.0.1:1/v1", "auth-key":"fixture-not-a-real-key"}
    }).to_string()).unwrap();
    let store = SessionStore::load_at(&SessionId::parse("persisted").unwrap(), root).unwrap();
    let reserved = store.start_request(Some(1), None, "hello", root).unwrap();
    store
        .finalize(
            &reserved,
            "done",
            &[RoundRecord {
                thinking: String::new(),
                continuation: None,
                assistant: "done".into(),
                calls: Vec::new(),
            }],
        )
        .unwrap();
}

fn tree(root: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(tree(&path));
        } else {
            files.push((path.clone(), std::fs::read(path).unwrap()));
        }
    }
    files.sort();
    files
}

#[test]
fn success_replay_and_all_error_exits_declare_persisted_format() {
    let root = tempfile::tempdir().unwrap();
    seed(root.path());
    let common = [
        "--session",
        "persisted",
        "--profile",
        "fixture",
        "--turn",
        "1",
        "-p",
        "hello",
    ];
    let output = bin(root.path()).args(common).output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["session_state_version"], STORE_VERSION);
    assert_eq!(value["replayed"], true);
    // Reservation failure, setup failure, Clap failure, CLI validation failure,
    // profile initialization failure and a live provider failure all share the field.
    for (args, code) in [
        (vec!["--branch", "missing"], 6),
        (vec!["--profile-load", "missing.json"], 2),
        (vec!["--unknown"], 2),
        (vec!["--max-tool-calls", "0"], 2),
        (vec!["--mem-profile", "missing-dir/profile.jsonl"], 7),
    ] {
        let output = bin(root.path()).args(common).args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(code), "{output:?}");
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["status"], "error");
        assert_eq!(value["session_state_version"], STORE_VERSION, "{value}");
    }
    let output = bin(root.path())
        .args([
            "--session",
            "persisted",
            "--profile",
            "fixture",
            "--turn",
            "2",
            "-p",
            "new prompt",
            "--request-timeout",
            "50ms",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(5), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["session_state_version"], STORE_VERSION);
}

#[test]
fn fresh_setup_error_does_not_claim_persisted_state_or_create_it() {
    let root = tempfile::tempdir().unwrap();
    let output = bin(root.path())
        .args(["--session", "fresh", "-p", "hello", "--profile", "missing"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value.get("session_state_version").is_none());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn corrupt_log_error_still_identifies_the_persisted_snapshot_without_repair() {
    let root = tempfile::tempdir().unwrap();
    seed(root.path());
    let dir = root.path().join("code-rs-sessions/persisted");
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(dir.join("session.manifest.json")).unwrap()).unwrap();
    let segment = dir.join(manifest["current"]["segment"].as_str().unwrap());
    std::fs::write(segment, b"invalid log").unwrap();
    let before = tree(root.path());
    let output = bin(root.path())
        .args(["--session", "persisted", "--unknown"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["session_state_version"], STORE_VERSION);
    assert_eq!(tree(root.path()), before);
}

#[test]
fn busy_session_error_declares_version_without_waiting_for_lock() {
    let root = tempfile::tempdir().unwrap();
    seed(root.path());
    let file = std::fs::File::open(root.path().join("code-rs-sessions/persisted/.lock")).unwrap();
    fs2::FileExt::lock_exclusive(&file).unwrap();
    let output = bounded_output(bin(root.path()).args(["--session", "persisted", "--unknown"]));
    assert_eq!(output.status.code(), Some(2));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["session_state_version"], STORE_VERSION);
}
