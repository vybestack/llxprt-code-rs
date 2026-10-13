//! Public-boundary regressions for issue 67. Every persistent root is injected;
//! these tests neither mutate process configuration nor construct a provider.

use llxprt_code_rs::limits::{
    validate_timeout, TIMEOUT_LEASE_MARGIN_SECONDS, TIMEOUT_LEASE_SECONDS,
};
use llxprt_code_rs::session::{Lifecycle, RoundRecord, SessionId, SessionStore, StoreError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[test]
fn reservation_renewal_and_checkpoint_use_the_timeout_policy_lease() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let id = SessionId::parse("lease-boundary").unwrap();
    let store = SessionStore::load_at(&id, &root.path().join("config")).unwrap();
    let reserved = store
        .start_request(None, None, "prompt", &workspace)
        .unwrap();
    let state = store.snapshot().unwrap();
    let branch = &state.branches[0];
    assert_eq!(branch.lifecycle, Lifecycle::Pending);
    assert_eq!(
        branch.lease_expiry - branch.reserved_at,
        TIMEOUT_LEASE_SECONDS
    );

    // Put a valid but shorter lease in the durable store. Each operation must
    // replace it using the leaf's policy, not merely preserve the initial reservation.
    for checkpoint in [false, true] {
        let mut state = store.snapshot().unwrap();
        state.branches[0].reserved_at = now() - 1;
        state.branches[0].lease_expiry = now() + 60;
        store.replace_snapshot(&state).unwrap();
        let before = now();
        if checkpoint {
            store.checkpoint(&reserved, &[]).unwrap();
        } else {
            store.renew_lease(&reserved).unwrap();
        }
        let after = now();
        let expiry = store.snapshot().unwrap().branches[0].lease_expiry;
        assert!((before + TIMEOUT_LEASE_SECONDS..=after + TIMEOUT_LEASE_SECONDS).contains(&expiry));
    }

    let boundary = TIMEOUT_LEASE_SECONDS - TIMEOUT_LEASE_MARGIN_SECONDS;
    assert!(validate_timeout(Some(Duration::from_secs(boundary - 1))).is_ok());
    assert!(validate_timeout(Some(Duration::from_secs(boundary))).is_err());
    assert!(validate_timeout(Some(Duration::from_secs(TIMEOUT_LEASE_SECONDS))).is_err());
}

#[test]
fn identical_session_ids_in_injected_roots_do_not_share_state() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let id = SessionId::parse("same-id").unwrap();
    let a = SessionStore::load_at(&id, first.path()).unwrap();
    let b = SessionStore::load_at(&id, second.path()).unwrap();
    let reserved = a.start_request(None, None, "first", first.path()).unwrap();
    assert!(b.snapshot().unwrap().branches.is_empty());
    assert!(matches!(
        a.start_request(None, None, "first", first.path()),
        Err(StoreError::Busy(_))
    ));
    a.finalize(
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
    drop(a);

    let reopened = SessionStore::load_at(&id, first.path()).unwrap();
    assert!(
        reopened
            .start_request(Some(1), None, "first", first.path())
            .unwrap()
            .replay
    );
    assert_eq!(reopened.snapshot().unwrap().branches[0].summary, "done");
    assert!(b.snapshot().unwrap().branches.is_empty());
}
