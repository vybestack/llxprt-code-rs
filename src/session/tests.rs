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
    assert_eq!(next_store_generation(None).unwrap(), 0);
    assert_eq!(next_store_generation(Some(41)).unwrap(), 42);
    assert!(matches!(
        next_store_generation(Some(u64::MAX)),
        Err(StoreError::Corrupt(_))
    ));
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
