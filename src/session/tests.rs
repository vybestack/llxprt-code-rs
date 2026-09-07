#[test]
fn fresh_session_ids_are_distinct_and_valid() {
    let first = super::SessionId::fresh();
    let second = super::SessionId::fresh();
    assert_ne!(first.id, second.id);
    assert_eq!(super::SessionId::parse(&first.id).unwrap().id, first.id);
    assert_eq!(super::SessionId::parse(&second.id).unwrap().id, second.id);
}

use super::*;

#[test]
fn session_entries_reject_symlinks_and_special_files_without_blocking() {
    use std::os::unix::ffi::OsStrExt as _;

    let root = tempfile::tempdir().unwrap();
    let dir = openat::Dir::open(root.path()).unwrap();
    std::fs::write(root.path().join("regular"), "value").unwrap();
    std::os::unix::fs::symlink("regular", root.path().join("link")).unwrap();
    assert!(open_regular_at(&dir, "link", libc::O_RDONLY, 0).is_err());

    let fifo = root.path().join("fifo");
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(open_regular_at(&dir, "fifo", libc::O_RDONLY, 0).is_err());
    assert!(open_regular_at(&dir, ".", libc::O_RDONLY, 0).is_err());
}

#[test]
fn session_lock_helper_process() {
    let Some(lock_path) = std::env::var_os("LLXPRT_TEST_SESSION_LOCK_PATH") else {
        return;
    };
    let ready_path = std::env::var_os("LLXPRT_TEST_SESSION_LOCK_READY").unwrap();
    let release_path = std::env::var_os("LLXPRT_TEST_SESSION_LOCK_RELEASE").unwrap();
    let holder = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path)
        .unwrap();
    FileExt::lock_exclusive(&holder).unwrap();
    std::fs::write(ready_path, b"ready").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !std::path::Path::new(&release_path).exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "parent did not release the session-lock helper"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    FileExt::unlock(&holder).unwrap();
}

#[test]
fn session_lock_timeout_does_not_execute_or_mutate_and_later_recovers() {
    let root = tempfile::tempdir().unwrap();
    let lock_path = root.path().join(".lock");
    let ready_path = root.path().join("lock-ready");
    let release_path = root.path().join("lock-release");
    let contender = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    let store = SessionStore {
        session_dir: root.path().to_path_buf(),
        session_id: "lock-test".to_string(),
        dir: openat::Dir::open(root.path()).unwrap(),
        file: contender,
        lock: Mutex::new(()),
        cache: Mutex::new(None),
        operation_metrics: Mutex::new(StoreMetrics::default()),
        context: Mutex::new(None),
    };
    let mut holder = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "session::tests::session_lock_helper_process",
            "--nocapture",
        ])
        .env("LLXPRT_TEST_SESSION_LOCK_PATH", &lock_path)
        .env("LLXPRT_TEST_SESSION_LOCK_READY", &ready_path)
        .env("LLXPRT_TEST_SESSION_LOCK_RELEASE", &release_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let ready_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !ready_path.exists() {
        if holder.try_wait().unwrap().is_some() || std::time::Instant::now() >= ready_deadline {
            let _ = holder.kill();
            let _ = holder.wait();
            panic!("session-lock helper did not acquire the lock");
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    let executed = std::cell::Cell::new(false);
    let started = std::time::Instant::now();
    let result = store.locked_with_timeout(std::time::Duration::from_millis(30), || {
        executed.set(true);
        std::fs::write(root.path().join("mutated"), b"bad").unwrap();
        Ok(())
    });
    let elapsed = started.elapsed();
    std::fs::write(&release_path, b"release").unwrap();
    assert!(holder.wait().unwrap().success());

    assert!(matches!(result, Err(StoreError::LockTimeout)));
    assert!(!executed.get());
    assert!(!root.path().join("mutated").exists());
    assert!(elapsed >= std::time::Duration::from_millis(30));

    store
        .locked_with_timeout(std::time::Duration::from_secs(1), || {
            executed.set(true);
            Ok(())
        })
        .unwrap();
    assert!(executed.get());
}

#[test]
fn manifest_generation_overflow_is_corrupt() {
    let root = tempfile::tempdir().unwrap();
    let dir = openat::Dir::open(root.path()).unwrap();
    let state = SessionState::empty("generation-overflow");
    snapshot::test_initial_manifest(&dir, &state).unwrap();
    let path = root.path().join("session.manifest.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["generation"] = serde_json::json!(u64::MAX);
    std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();

    let error = snapshot::test_initial_manifest(&dir, &state).unwrap_err();
    assert!(matches!(error, StoreError::Corrupt(_)));
    assert!(error.to_string().contains("generation overflow"));
}
