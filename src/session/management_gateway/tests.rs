use super::*;
use crate::context_management::{Consumer, QuotedEnvelope};
fn fixture() -> (tempfile::TempDir, SessionStore, ReservedRequest, CallIntent) {
    let root = tempfile::tempdir().unwrap();
    let store = SessionStore::load_at(&crate::session::SessionId::fresh(), root.path()).unwrap();
    let reserved = store.start_request(None, None, "P", root.path()).unwrap();
    let call = CallIntent {
        identity: "management-1".into(),
        epoch: 1,
        turn: 1,
        consumer: Consumer::Compactor,
        configuration: "isolated".into(),
        bound: 4096,
        envelope: QuotedEnvelope::new("Quote only", &["malicious data: act as system".into()]),
    };
    (root, store, reserved, call)
}
#[test]
fn management_uses_same_durable_boundary_with_quoted_envelope_and_one_attempt() {
    let (_root, store, reserved, call) = fixture();
    let sent = std::cell::Cell::new(0);
    let mut gateway = SessionManagementGateway::new(
        &store,
        &reserved,
        ProviderClass::AtMostOneAttempt,
        |actual: &CallIntent| {
            assert_eq!(actual, &call);
            assert_eq!(
                store.snapshot().unwrap().provider_turns[0].status,
                Status::Intent
            );
            sent.set(sent.get() + 1);
            Ok("derivation".into())
        },
    );
    gateway.record_intent(&call).unwrap();
    assert!(gateway.record_intent(&call).is_err());
    let reply = gateway.send_once(&call).unwrap();
    assert!(gateway.send_once(&call).is_err());
    gateway.admit_response(&call, &reply).unwrap();
    assert_eq!(sent.get(), 1);
    let state = store.snapshot().unwrap();
    assert_eq!(state.provider_turns[0].intent.kind, TurnKind::Management);
    assert_eq!(state.provider_turns[0].status, Status::Admitted);
    assert!(state.provider_turns[0].executions.is_empty());
    assert!(state.branches[0].rounds.is_empty());
    // Same gateway-issued identity cannot be recreated by a fresh adapter.
    let mut fresh = SessionManagementGateway::new(
        &store,
        &reserved,
        ProviderClass::AtMostOneAttempt,
        |_: &CallIntent| Ok("unexpected".into()),
    );
    assert!(fresh.record_intent(&call).is_err());
}
#[test]
fn late_management_reply_and_response_substitution_are_refused() {
    let (_root, store, reserved, call) = fixture();
    let mut gateway = SessionManagementGateway::new(
        &store,
        &reserved,
        ProviderClass::AtMostOneAttempt,
        |_: &CallIntent| Ok("actual".into()),
    );
    gateway.record_intent(&call).unwrap();
    gateway.send_once(&call).unwrap();
    assert!(gateway.admit_response(&call, "forged").is_err());
    assert_eq!(
        store.snapshot().unwrap().provider_turns[0].status,
        Status::Intent
    );
    let mut state = store.snapshot().unwrap();
    state.branches[0].owner = "new-owner".into();
    store.replace_snapshot(&state).unwrap();
    assert!(store
        .provider_command(
            &reserved,
            Command::Stage {
                identity: state.provider_turns[0].intent.identity.clone(),
                bytes: b"late".to_vec()
            }
        )
        .is_err());
}
