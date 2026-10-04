//! Strict destination ownership and fail-fast CLI errors, without live credentials.
use serde_json::Value;
use std::path::Path;
use std::process::{Command, Output};

fn invoke(root: &Path, destination: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
        .env("LLXPRT_CONFIG_HOME", root)
        .args([
            "--profile",
            "missing",
            "--session",
            "strict",
            "--cache-observations",
        ])
        .arg(destination)
        .args(args)
        .output()
        .unwrap()
}

fn assert_open_error(output: &Output, secret: &str) {
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stderr.is_empty());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["code"], "cache-output-open");
    assert!(value["error"]["message"].as_str().unwrap().len() < 256);
    assert!(!String::from_utf8_lossy(&output.stdout).contains(secret));
}

#[test]
fn unsafe_existing_destinations_fail_before_profile_stdin_or_session_mutation() {
    use std::os::unix::ffi::OsStrExt;
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("nonexistent-root");
    let existing = work.path().join("private-secret-file");
    std::fs::write(&existing, "untouched sentinel").unwrap();
    let directory = work.path().join("directory");
    std::fs::create_dir(&directory).unwrap();
    let link = work.path().join("link");
    std::os::unix::fs::symlink(&existing, &link).unwrap();
    let fifo = work.path().join("fifo");
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    for destination in [
        &existing,
        &directory,
        &link,
        &fifo,
        Path::new("/dev/stderr"),
        Path::new("/dev/stdout"),
    ] {
        assert_open_error(&invoke(&root, destination, &[]), "private-secret-file");
        assert!(!root.exists());
        assert_eq!(
            std::fs::read_to_string(&existing).unwrap(),
            "untouched sentinel"
        );
    }
}

#[test]
fn same_memory_and_cache_destination_is_explicit_failure_without_stream_mix() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("config");
    let path = work.path().join("owners.jsonl");
    let output = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
        .env("LLXPRT_CONFIG_HOME", &root)
        .args([
            "--profile",
            "missing",
            "--session",
            "collision",
            "--emit",
            "all",
            "-p",
            "hello",
            "--mem-profile",
        ])
        .arg(&path)
        .arg("--cache-observations")
        .arg(&path)
        .output()
        .unwrap();
    assert_open_error(&output, "owners.jsonl");
    assert!(!root.exists());
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(!content.contains("prompt_cache_"));
    for line in content.lines() {
        let value: Value = serde_json::from_str(line).unwrap();
        assert!(value.get("phase").is_some());
    }
}

#[test]
fn read_only_and_nonruntime_modes_never_open_a_cache_sink() {
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("never-created.jsonl");
    for args in [
        vec!["--print-config"],
        vec!["transcript", "--session", "missing", "--json"],
        vec!["--localoauth", "--oauth-login"],
    ] {
        let output = invoke(work.path(), &path, &args);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stderr.is_empty());
        let _: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(!path.exists());
        assert_eq!(std::fs::read_dir(work.path()).unwrap().count(), 0);
    }
    let help = invoke(work.path(), &path, &["--help"]);
    assert!(help.status.success());
    assert!(String::from_utf8(help.stdout)
        .unwrap()
        .contains("--cache-observations <PATH>"));
    assert!(!path.exists());
}

#[test]
fn external_transcript_redirection_collision_does_not_truncate_or_mix() {
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("transcript.jsonl");
    std::fs::write(&path, "external owner sentinel\n").unwrap();
    let stderr = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
        .env("LLXPRT_CONFIG_HOME", work.path())
        .args([
            "--session",
            "strict",
            "--emit",
            "all",
            "--cache-observations",
        ])
        .arg(&path)
        .stderr(stderr)
        .output()
        .unwrap();
    assert_open_error(&output, "transcript.jsonl");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "external owner sentinel\n"
    );
    assert_eq!(std::fs::read_dir(work.path()).unwrap().count(), 1);
}
