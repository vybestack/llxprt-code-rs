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
