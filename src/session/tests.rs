#[cfg(test)]
fn write_state_slot(dir: &openat::Dir, name: &str, bytes: &[u8]) -> Result<(), StoreError> {
    write_state_slot_inner(
        dir,
        name,
        bytes,
        || {},
        |fd| {
            if unsafe { libc::fsync(fd) } == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        },
    )
}

#[cfg(test)]
fn write_state_slot_inner(
    dir: &openat::Dir,
    name: &str,
    bytes: &[u8],
    after_open: impl FnOnce(),
    sync_directory: impl FnOnce(std::os::fd::RawFd) -> std::io::Result<()>,
) -> Result<(), StoreError> {
    use std::io::{Seek as _, Write as _};

    if bytes.len() > MAX_SESSION_BYTES {
        return Err(StoreError::Invalid(format!(
            "session state exceeds the {MAX_SESSION_BYTES} byte cap"
        )));
    }
    use std::os::fd::AsRawFd as _;

    let mut f = open_regular_at(dir, name, libc::O_RDWR | libc::O_CREAT, 0o600)
        .map_err(|_| StoreError::Io("open session state slot failed".into()))?;
    after_open();
    fchmod(f.as_raw_fd(), 0o600)?;
    f.set_len(0)
        .map_err(|_| StoreError::Io("truncate session state slot failed".into()))?;
    f.seek(std::io::SeekFrom::Start(0))
        .map_err(|_| StoreError::Io("seek session state slot failed".into()))?;
    f.write_all(bytes)
        .map_err(|_| StoreError::Io("write session state slot failed".into()))?;
    f.sync_all()
        .map_err(|_| StoreError::Io("sync session state slot failed".into()))?;
    let installed = open_regular_at(dir, name, libc::O_RDONLY, 0)
        .map_err(|_| StoreError::Io("verify session state slot failed".into()))?;
    if !same_file_identity(&f, &installed)? {
        let _ = f.set_len(0);
        let _ = f.sync_all();
        return Err(StoreError::Io(
            "session state slot name was replaced".into(),
        ));
    }
    let syncable_dir = dir
        .open_file(".")
        .map_err(|_| StoreError::InstalledDurabilityUnknown)?;
    sync_directory(syncable_dir.as_raw_fd()).map_err(|_| StoreError::InstalledDurabilityUnknown)?;
    let installed = open_regular_at(dir, name, libc::O_RDONLY, 0)
        .map_err(|_| StoreError::InstalledDurabilityUnknown)?;
    if !same_file_identity(&f, &installed)? {
        let _ = f.set_len(0);
        let _ = f.sync_all();
        return Err(StoreError::InstalledDurabilityUnknown);
    }
    Ok(())
}

use super::*;

#[test]
fn state_io_stays_on_retained_directory_after_parent_substitution() {
    let root = tempfile::tempdir().unwrap();
    let named = root.path().join("named");
    let moved = root.path().join("moved");
    std::fs::create_dir(&named).unwrap();
    let dir = openat::Dir::open(&named).unwrap();
    std::fs::rename(&named, &moved).unwrap();
    std::fs::create_dir(&named).unwrap();

    write_state_slot(&dir, "session.json", br#"{"payload":true}"#).unwrap();
    assert_eq!(
        std::fs::read(moved.join("session.json")).unwrap(),
        br#"{"payload":true}"#
    );
    assert!(!named.join("session.json").exists());
}

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
fn dual_slots_recover_from_an_interrupted_newer_write() {
    let root = tempfile::tempdir().unwrap();
    let dir = openat::Dir::open(root.path()).unwrap();
    let state = SessionState::empty("recoverable");
    let primary = StateSlot {
        store_generation: 2,
        state: state.clone(),
    };
    write_state_slot(&dir, "session.json", &serde_json::to_vec(&primary).unwrap()).unwrap();
    std::fs::write(root.path().join("session.alt.json"), b"interrupted").unwrap();

    let (generation, recovered) = read_state_with_generation(&dir).unwrap().unwrap();
    assert_eq!(generation, 2);
    assert_eq!(recovered.session_id, "recoverable");
}

fn write_valid_slot(dir: &openat::Dir, name: &str, generation: u64, session_id: &str) {
    let slot = StateSlot {
        store_generation: generation,
        state: SessionState::empty(session_id),
    };
    write_state_slot(dir, name, &serde_json::to_vec(&slot).unwrap()).unwrap();
}

fn write_oversized_slot(path: &std::path::Path) {
    std::fs::File::create(path)
        .unwrap()
        .set_len(MAX_SESSION_BYTES as u64 + 1)
        .unwrap();
}

#[test]
fn valid_primary_recovers_from_oversized_alternate() {
    let root = tempfile::tempdir().unwrap();
    let dir = openat::Dir::open(root.path()).unwrap();
    write_valid_slot(&dir, "session.json", 3, "primary");
    write_oversized_slot(&root.path().join("session.alt.json"));

    let (generation, state) = read_state_with_generation(&dir).unwrap().unwrap();
    assert_eq!(generation, 3);
    assert_eq!(state.session_id, "primary");
}

#[test]
fn valid_alternate_recovers_from_oversized_primary() {
    let root = tempfile::tempdir().unwrap();
    let dir = openat::Dir::open(root.path()).unwrap();
    write_oversized_slot(&root.path().join("session.json"));
    write_valid_slot(&dir, "session.alt.json", 4, "alternate");

    let (generation, state) = read_state_with_generation(&dir).unwrap().unwrap();
    assert_eq!(generation, 4);
    assert_eq!(state.session_id, "alternate");
}

#[test]
fn two_invalid_slots_return_concrete_corruption() {
    let root = tempfile::tempdir().unwrap();
    let dir = openat::Dir::open(root.path()).unwrap();
    write_oversized_slot(&root.path().join("session.json"));
    std::fs::write(root.path().join("session.alt.json"), b"malformed").unwrap();

    let error = read_state_with_generation(&dir).unwrap_err();
    assert!(error.to_string().contains("session byte cap"));
}

#[test]
fn substituted_slot_name_preserves_victim_and_clears_retained_private_bytes() {
    let root = tempfile::tempdir().unwrap();
    let dir = openat::Dir::open(root.path()).unwrap();
    let slot = root.path().join("session.json");
    let moved = root.path().join("moved-slot");
    let result = write_state_slot_inner(
        &dir,
        "session.json",
        b"private state",
        || {
            std::fs::rename(&slot, &moved).unwrap();
            std::fs::write(&slot, b"replacement victim").unwrap();
        },
        |fd| {
            if unsafe { libc::fsync(fd) } == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        },
    );

    assert!(result.is_err());
    assert_eq!(std::fs::read(slot).unwrap(), b"replacement victim");
    assert_eq!(std::fs::metadata(moved).unwrap().len(), 0);
}

#[test]
fn directory_sync_failure_reports_installed_durability_unknown() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let root = tempfile::tempdir().unwrap();
    let dir = openat::Dir::open(root.path()).unwrap();
    let error = write_state_slot_inner(
        &dir,
        "session.json",
        b"installed state",
        || {},
        |_| Err(std::io::Error::other("injected directory sync failure")),
    )
    .unwrap_err();

    assert!(matches!(error, StoreError::InstalledDurabilityUnknown));
    let installed = root.path().join("session.json");
    assert_eq!(std::fs::read(&installed).unwrap(), b"installed state");
    let metadata = std::fs::metadata(installed).unwrap();
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    assert_ne!(metadata.ino(), 0);
}

#[test]
fn state_slot_byte_cap_accepts_exactly_the_cap_and_rejects_plus_one() {
    let root = tempfile::tempdir().unwrap();
    let dir = openat::Dir::open(root.path()).unwrap();
    let mut bytes = vec![b'x'; MAX_SESSION_BYTES];
    write_state_slot(&dir, "session.json", &bytes).unwrap();
    assert_eq!(
        std::fs::metadata(root.path().join("session.json"))
            .unwrap()
            .len(),
        MAX_SESSION_BYTES as u64
    );

    bytes.push(b'x');
    assert!(matches!(
        write_state_slot(&dir, "session.json", &bytes),
        Err(StoreError::Invalid(_))
    ));
    assert_eq!(
        std::fs::metadata(root.path().join("session.json"))
            .unwrap()
            .len(),
        MAX_SESSION_BYTES as u64
    );
}

#[test]
fn substitution_during_directory_sync_is_detected_without_deleting_victim() {
    let root = tempfile::tempdir().unwrap();
    let dir = openat::Dir::open(root.path()).unwrap();
    let slot = root.path().join("session.json");
    let moved = root.path().join("moved-slot");
    let result = write_state_slot_inner(
        &dir,
        "session.json",
        b"private state",
        || {},
        |_| {
            std::fs::rename(&slot, &moved).unwrap();
            std::fs::write(&slot, b"replacement victim").unwrap();
            Ok(())
        },
    );

    assert!(matches!(
        result,
        Err(StoreError::InstalledDurabilityUnknown)
    ));
    assert_eq!(std::fs::read(slot).unwrap(), b"replacement victim");
    assert_eq!(std::fs::metadata(moved).unwrap().len(), 0);
}

#[test]
fn both_corrupt_slots_preserve_a_concrete_corruption_error() {
    let root = tempfile::tempdir().unwrap();
    let dir = openat::Dir::open(root.path()).unwrap();
    std::fs::write(root.path().join("session.json"), b"primary broken").unwrap();
    std::fs::write(root.path().join("session.alt.json"), b"alternate broken").unwrap();

    let error = read_state_with_generation(&dir).unwrap_err();
    assert!(matches!(error, StoreError::Corrupt(_)));
    assert!(error.to_string().contains("not valid JSON"));
}

#[test]
fn generation_overflow_fails_before_selecting_a_slot() {
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

#[test]
fn first_slot_open_failure_does_not_create_or_modify_state() {
    let root = tempfile::tempdir().unwrap();
    let dir = openat::Dir::open(root.path()).unwrap();
    std::fs::create_dir(root.path().join("session.json")).unwrap();

    assert!(write_state_slot(&dir, "session.json", b"private state").is_err());
    assert!(root.path().join("session.json").is_dir());
    assert!(!root.path().join("session.alt.json").exists());
}
