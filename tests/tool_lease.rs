//! A still-owned expired lease is not a concurrent writer. No hour-long sleep needed.
use llxprt_code_rs::session::{SessionId, SessionStore};

#[test]
fn owned_expiration_reports_elapsed_time_without_changing_state() {
    let root = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let store = SessionStore::load_at(&SessionId::fresh(), root.path()).unwrap();
    let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    let mut state = store.snapshot().unwrap();
    state.branches[0].reserved_at = 1;
    state.branches[0].lease_expiry = 2;
    store.replace_snapshot(&state).unwrap();
    let before = serde_json::to_value(store.snapshot().unwrap()).unwrap();
    let error = store.finalize(&reserved, "unwritten", &[]).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("lease expired during tool execution"),
        "{error}"
    );
    assert!(error.to_string().contains("elapsed"), "{error}");
    assert_eq!(
        serde_json::to_value(store.snapshot().unwrap()).unwrap(),
        before
    );
}

#[test]
fn mismatched_reservation_is_stale_even_if_lease_expired() {
    let root = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let store = SessionStore::load_at(&SessionId::fresh(), root.path()).unwrap();
    let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    let mut state = store.snapshot().unwrap();
    state.branches[0].reserved_at = 1;
    state.branches[0].lease_expiry = 2;
    store.replace_snapshot(&state).unwrap();
    for field in ["owner", "turn", "attempt", "prompt"] {
        let mut wrong = reserved.clone();
        match field {
            "owner" => wrong.owner.push_str("-other"),
            "turn" => wrong.turn += 1,
            "attempt" => wrong.attempt += 1,
            "prompt" => wrong.prompt.push_str("-other"),
            _ => unreachable!(),
        }
        assert!(
            matches!(
                store.finalize(&wrong, "unwritten", &[]),
                Err(llxprt_code_rs::session::StoreError::Stale)
            ),
            "{field}"
        );
    }
}
