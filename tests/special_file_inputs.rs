#![cfg(unix)]

use std::os::unix::ffi::OsStrExt as _;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

fn run_bounded(mut command: Command) -> Output {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("CLI blocked while opening a special file");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn command(config: &std::path::Path, profile: &str, session: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"));
    command
        .env("LLXPRT_CONFIG_HOME", config)
        .arg("--profile")
        .arg(profile)
        .arg("--session")
        .arg(session)
        .arg("--cwd")
        .arg(config)
        .arg("-p")
        .arg("must fail before a provider request");
    command
}

fn mkfifo(path: &std::path::Path) {
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
}

fn write_profile(path: &std::path::Path, keyfile: Option<&std::path::Path>) {
    let auth = match keyfile {
        Some(path) => serde_json::json!({"auth-keyfile": path}),
        None => serde_json::json!({}),
    };
    std::fs::write(
        path,
        serde_json::json!({
            "provider": "openai",
            "model": "special-file-test",
            "ephemeralSettings": auth,
        })
        .to_string(),
    )
    .unwrap();
}

#[test]
fn profile_and_keyfile_special_entries_fail_without_blocking() {
    let temp = tempfile::tempdir().unwrap();
    let profiles = temp.path().join("profiles");
    std::fs::create_dir(&profiles).unwrap();

    let profile_fifo = profiles.join("fifo.json");
    mkfifo(&profile_fifo);
    assert!(!run_bounded(command(temp.path(), "fifo", "profile-fifo"))
        .status
        .success());

    std::fs::create_dir(profiles.join("directory.json")).unwrap();
    assert!(
        !run_bounded(command(temp.path(), "directory", "profile-dir"))
            .status
            .success()
    );

    std::os::unix::fs::symlink("missing", profiles.join("dangling.json")).unwrap();
    assert!(
        !run_bounded(command(temp.path(), "dangling", "profile-dangling"))
            .status
            .success()
    );

    let regular = profiles.join("regular.json");
    write_profile(&regular, Some(std::path::Path::new("/dev/null")));
    std::os::unix::fs::symlink(&regular, profiles.join("symlink.json")).unwrap();
    assert!(
        !run_bounded(command(temp.path(), "symlink", "profile-link"))
            .status
            .success()
    );
    assert!(!run_bounded(command(temp.path(), "regular", "key-device"))
        .status
        .success());

    let key_fifo = temp.path().join("key-fifo");
    mkfifo(&key_fifo);
    write_profile(&profiles.join("keyfifo.json"), Some(&key_fifo));
    assert!(!run_bounded(command(temp.path(), "keyfifo", "key-fifo"))
        .status
        .success());

    let socket = temp.path().join("key-socket");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    write_profile(&profiles.join("keysocket.json"), Some(&socket));
    assert!(
        !run_bounded(command(temp.path(), "keysocket", "key-socket"))
            .status
            .success()
    );
}

#[test]
fn settings_and_session_special_entries_fail_without_blocking() {
    let temp = tempfile::tempdir().unwrap();
    let profiles = temp.path().join("profiles");
    std::fs::create_dir(&profiles).unwrap();
    write_profile(&profiles.join("noauth.json"), None);
    mkfifo(&temp.path().join("settings.json"));
    assert!(
        !run_bounded(command(temp.path(), "noauth", "settings-fifo"))
            .status
            .success()
    );

    std::fs::remove_file(temp.path().join("settings.json")).unwrap();
    let profile = profiles.join("valid.json");
    std::fs::write(
        &profile,
        r#"{"provider":"openai","model":"special-file-test","ephemeralSettings":{"auth-key":"test-only-key","base-url":"http://127.0.0.1:1"}}"#,
    )
    .unwrap();
    let session_dir = temp.path().join("code-rs-sessions").join("bad-session");
    std::fs::create_dir_all(&session_dir).unwrap();
    mkfifo(&session_dir.join(".lock"));
    assert!(!run_bounded(command(temp.path(), "valid", "bad-session"))
        .status
        .success());

    std::fs::remove_file(session_dir.join(".lock")).unwrap();
    std::fs::write(session_dir.join(".lock"), "").unwrap();
    mkfifo(&session_dir.join("session.json"));
    assert!(!run_bounded(command(temp.path(), "valid", "bad-session"))
        .status
        .success());

    std::fs::remove_file(session_dir.join("session.json")).unwrap();
    std::fs::remove_file(session_dir.join(".lock")).unwrap();
    std::fs::write(session_dir.join("lock-target"), "").unwrap();
    std::os::unix::fs::symlink("lock-target", session_dir.join(".lock")).unwrap();
    assert!(!run_bounded(command(temp.path(), "valid", "bad-session"))
        .status
        .success());

    std::fs::remove_file(session_dir.join(".lock")).unwrap();
    std::fs::write(session_dir.join(".lock"), "").unwrap();
    std::fs::write(session_dir.join("state-target"), "{}").unwrap();
    std::os::unix::fs::symlink("state-target", session_dir.join("session.json")).unwrap();
    assert!(!run_bounded(command(temp.path(), "valid", "bad-session"))
        .status
        .success());

    std::fs::remove_file(session_dir.join("session.json")).unwrap();
    mkfifo(&session_dir.join("session.alt.json"));
    assert!(!run_bounded(command(temp.path(), "valid", "bad-session"))
        .status
        .success());
}
