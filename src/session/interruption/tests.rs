use super::*;
use crate::session::{SessionId, SessionStore};

fn fixture() -> (tempfile::TempDir, SessionStore, ReservedRequest) {
    let root = tempfile::tempdir().unwrap();
    let store = SessionStore::load_at(&SessionId::fresh(), root.path()).unwrap();
    let reserved = store.start_request(None, None, "P", root.path()).unwrap();
    (root, store, reserved)
}

fn signal(store: &SessionStore, reserved: &ReservedRequest, bytes: &[u8]) {
    std::fs::write(
        store.session_dir().join(name(&reserved.owner, "signal")),
        bytes,
    )
    .unwrap();
}

#[test]
fn cancellation_without_tool_and_cleared_tool_are_explicit() {
    let (_root, store, reserved) = fixture();
    let branch = &store.snapshot().unwrap().branches[0];
    assert!(read(store.session_dir(), branch).unwrap().is_none());
    signal(&store, &reserved, b"");
    assert!(read(store.session_dir(), branch).unwrap().is_none());
    signal(&store, &reserved, b"T");
    let cancellation = read(store.session_dir(), branch).unwrap().unwrap();
    assert_eq!(cancellation.signal, "SIGTERM");
    assert!(cancellation.in_flight.is_none());
    let flight = InFlight {
        tool: "list_directory".into(),
        id: "list".into(),
        turn: 1,
        round: 3,
        started: 42,
        args: "{\"path\":\".\"}".into(),
    };
    mark(&store, &reserved, &flight).unwrap();
    assert_eq!(
        read(store.session_dir(), branch)
            .unwrap()
            .unwrap()
            .in_flight,
        Some(flight)
    );
    clear(&store, &reserved).unwrap();
    assert!(!store.session_dir().join("tool-in-flight.json").exists());
    assert!(read(store.session_dir(), branch)
        .unwrap()
        .unwrap()
        .in_flight
        .is_none());
}

#[test]
fn markers_are_owner_scoped_and_do_not_override_terminal_history() {
    let (_root, store, reserved) = fixture();
    let mut branch = store.snapshot().unwrap().branches[0].clone();
    signal(&store, &reserved, b"I");
    assert_eq!(
        read(store.session_dir(), &branch).unwrap().unwrap().signal,
        "SIGINT"
    );
    branch.owner = "a-different-reservation".into();
    assert!(read(store.session_dir(), &branch).unwrap().is_none());
    branch.owner = reserved.owner;
    branch.lifecycle = crate::session::Lifecycle::Completed;
    assert!(read(store.session_dir(), &branch).unwrap().is_none());
}

#[test]
fn malformed_oversized_and_symlinked_evidence_is_not_treated_as_absent() {
    let (_root, store, reserved) = fixture();
    let branch = &store.snapshot().unwrap().branches[0];
    signal(&store, &reserved, b"garbage");
    assert!(read(store.session_dir(), branch).is_err());
    signal(&store, &reserved, b"T");
    let path = tool_path(store.session_dir(), &reserved.owner);
    std::fs::write(&path, b"not json").unwrap();
    assert!(read(store.session_dir(), branch).is_err());
    let mut oversized = b"null".to_vec();
    oversized.resize(MAX_RECORD_BYTES + 1, b' ');
    std::fs::write(&path, oversized).unwrap();
    assert!(read(store.session_dir(), branch).is_err());
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink("absent", &path).unwrap();
    assert!(read(store.session_dir(), branch).is_err());
}

fn flight(id: &str) -> InFlight {
    InFlight {
        tool: "run_shell_command".into(),
        id: id.into(),
        turn: 1,
        round: 1,
        started: 42,
        args: "{\"command\":\"sleep 60\"}".into(),
    }
}

#[test]
fn new_invocation_retires_dead_projection_not_owner_evidence() {
    use std::os::unix::fs::PermissionsExt;
    let (root, store, first) = fixture();
    let registration = prepare(&store, &first).unwrap();
    mark(&store, &first, &flight("dead")).unwrap();
    signal(&store, &first, b"T");
    let evidence = std::fs::read(tool_path(store.session_dir(), &first.owner)).unwrap();
    assert_eq!(
        registration.metadata().unwrap().permissions().mode() & 0o777,
        0o600
    );
    drop(registration);
    let second = store
        .start_request(None, None, "continue", root.path())
        .unwrap();
    let _registration = prepare(&store, &second).unwrap();
    assert!(!store.session_dir().join("tool-in-flight.json").exists());
    assert_eq!(
        std::fs::read(tool_path(store.session_dir(), &first.owner)).unwrap(),
        evidence
    );
    let branch = &store.snapshot().unwrap().branches[0];
    let cancellation = read(store.session_dir(), branch).unwrap().unwrap();
    assert_eq!(cancellation.signal, "SIGTERM");
    assert_eq!(cancellation.in_flight, Some(flight("dead")));
}

#[test]
fn live_owner_projection_survives_another_invocations_prepare_and_clear() {
    let (root, store, first) = fixture();
    let registration = prepare(&store, &first).unwrap();
    mark(&store, &first, &flight("live")).unwrap();
    let path = store.session_dir().join("tool-in-flight.json");
    let before = std::fs::read(&path).unwrap();
    let second = store
        .start_request(None, None, "other prompt", root.path())
        .unwrap();
    let _second_registration = prepare(&store, &second).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), before);
    // No second tool may overwrite a live owner's current-tool projection.
    assert!(matches!(
        mark(&store, &second, &flight("other")),
        Err(StoreError::Io(_))
    ));
    clear(&store, &second).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), before);
    // The independent open descriptor's lock, not a PID or wall clock, controls retirement.
    drop(registration);
    let third = store
        .start_request(None, None, "later prompt", root.path())
        .unwrap();
    let _third_registration = prepare(&store, &third).unwrap();
    assert!(!path.exists());
    assert_eq!(
        serde_json::from_slice::<InFlight>(
            &std::fs::read(tool_path(store.session_dir(), &first.owner)).unwrap()
        )
        .unwrap(),
        flight("live")
    );
}

#[test]
fn reclaimed_owner_cannot_prepare_publish_or_clear() {
    let (root, store, first) = fixture();
    let _first_registration = prepare(&store, &first).unwrap();
    mark(&store, &first, &flight("first")).unwrap();
    let mut state = store.snapshot().unwrap();
    state.branches[0].reserved_at = 1;
    state.branches[0].lease_expiry = 2;
    store.replace_snapshot(&state).unwrap();
    let second = store.start_request(None, None, "P", root.path()).unwrap();
    let _second_registration = prepare(&store, &second).unwrap();
    // The old publisher is still executing: even reclaim does not erase its marker.
    let path = store.session_dir().join("tool-in-flight.json");
    let before = std::fs::read(&path).unwrap();
    let evidence = std::fs::read(tool_path(store.session_dir(), &first.owner)).unwrap();
    assert!(matches!(prepare(&store, &first), Err(StoreError::Stale)));
    assert!(matches!(
        mark(&store, &first, &flight("stale")),
        Err(StoreError::Stale)
    ));
    assert!(matches!(clear(&store, &first), Err(StoreError::Stale)));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(
        std::fs::read(tool_path(store.session_dir(), &first.owner)).unwrap(),
        evidence
    );
}

#[test]
fn replay_does_not_reconcile_or_arm() {
    let (_root, store, mut reserved) = fixture();
    let path = store.session_dir().join("tool-in-flight.json");
    std::fs::write(&path, b"untouched").unwrap();
    reserved.replay = true;
    assert!(arm(&store, &reserved).unwrap().is_none());
    assert_eq!(std::fs::read(&path).unwrap(), b"untouched");
    assert!(!store
        .session_dir()
        .join(name(&reserved.owner, "signal"))
        .exists());
}

#[test]
fn reconciliation_storage_failures_are_typed_and_preserve_projection() {
    for invalid in [
        b"not json".to_vec(),
        vec![b' '; MAX_RECORD_BYTES + 1],
        br#"{"tool":"shell"}"#.to_vec(),
    ] {
        let (_root, store, reserved) = fixture();
        let path = store.session_dir().join("tool-in-flight.json");
        std::fs::write(&path, &invalid).unwrap();
        assert!(matches!(prepare(&store, &reserved), Err(StoreError::Io(_))));
        assert_eq!(std::fs::read(&path).unwrap(), invalid);
    }
    let (_root, store, reserved) = fixture();
    let path = store.session_dir().join("tool-in-flight.json");
    std::os::unix::fs::symlink("absent", &path).unwrap();
    assert!(matches!(prepare(&store, &reserved), Err(StoreError::Io(_))));
    assert!(std::fs::symlink_metadata(&path)
        .unwrap()
        .file_type()
        .is_symlink());
}

#[test]
fn missing_or_unreadable_publisher_signal_is_not_proof_of_exit() {
    let (root, store, first) = fixture();
    mark(&store, &first, &flight("unknown")).unwrap();
    let path = store.session_dir().join("tool-in-flight.json");
    let before = std::fs::read(&path).unwrap();
    let second = store
        .start_request(None, None, "second", root.path())
        .unwrap();
    assert!(matches!(prepare(&store, &second), Err(StoreError::Io(_))));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    std::os::unix::fs::symlink(
        "absent",
        store.session_dir().join(name(&first.owner, "signal")),
    )
    .unwrap();
    let third = store
        .start_request(None, None, "third", root.path())
        .unwrap();
    assert!(matches!(prepare(&store, &third), Err(StoreError::Io(_))));
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
