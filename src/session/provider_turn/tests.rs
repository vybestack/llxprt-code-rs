use super::*;
use gateway::{ProviderClass, TurnKind};
fn fixture() -> (tempfile::TempDir, SessionStore, ReservedRequest) {
    let root = tempfile::tempdir().unwrap();
    let store = SessionStore::load_at(&SessionId::fresh(), root.path()).unwrap();
    let reserved = store.start_request(None, None, "P", root.path()).unwrap();
    (root, store, reserved)
}
fn intent(r: &ReservedRequest, kind: TurnKind) -> SendIntent {
    SendIntent {
        identity: "stable".into(),
        branch: r.branch_id.clone(),
        owner: r.owner.clone(),
        epoch: 1,
        context_version: 1,
        request_hash: gateway::digest(b"request"),
        contract_hash: gateway::digest(b"contract"),
        class: ProviderClass::AtMostOneAttempt,
        kind,
        allowance: 4096,
    }
}
fn reopen(root: &tempfile::TempDir, store: &SessionStore) -> SessionStore {
    SessionStore::load_at(
        &SessionId::parse(&store.snapshot().unwrap().session_id).unwrap(),
        root.path(),
    )
    .unwrap()
}
fn reclaim(root: &tempfile::TempDir, store: &SessionStore) -> (SessionStore, ReservedRequest) {
    let other = reopen(root, store);
    let mut snapshot = other.snapshot().unwrap();
    snapshot.branches[0].reserved_at = 1;
    snapshot.branches[0].lease_expiry = 2;
    other.replace_snapshot(&snapshot).unwrap();
    let reserved = other.start_request(None, None, "P", root.path()).unwrap();
    (other, reserved)
}
fn call(id: &str) -> ExecutionIntent {
    ExecutionIntent {
        id: id.into(),
        tool: "write_file".into(),
        args: "{}".into(),
        read_only: false,
        completion: None,
    }
}
#[test]
fn unresolved_send_and_pending_stage_never_authorize_reissue() {
    for stage in [false, true] {
        let (root, store, reserved) = fixture();
        let permit = store
            .provider_intent(&reserved, intent(&reserved, TurnKind::Conversation))
            .unwrap();
        let id = permit.intent().identity.clone();
        if stage {
            store
                .provider_command(
                    &reserved,
                    Command::Stage {
                        identity: id.clone(),
                        bytes: b"reply".to_vec(),
                    },
                )
                .unwrap();
        }
        let (fresh, new) = reclaim(&root, &store);
        assert_ne!(reserved.owner, new.owner);
        assert!(matches!(
            store.admit_send(&reserved, permit),
            Err(StoreError::Stale)
        ));
        assert!(fresh
            .recover_provider_turns(&new)
            .err()
            .unwrap()
            .to_string()
            .contains("indeterminate"));
        let record = reopen(&root, &fresh)
            .snapshot()
            .unwrap()
            .provider_turns
            .remove(0);
        assert_eq!(record.intent.identity, id);
        assert_eq!(record.status, Status::Indeterminate);
        assert!(record.response.is_none());
        assert!(record.executions.is_empty());
        assert!(fresh.recover_provider_turns(&new).is_err());
    }
}
#[test]
fn admission_and_tool_prefix_survive_without_repeating_effects() {
    let (root, store, reserved) = fixture();
    let id = store
        .provider_intent(&reserved, intent(&reserved, TurnKind::Conversation))
        .unwrap()
        .consume()
        .identity;
    store
        .admit_provider_response(
            &reserved,
            &id,
            b"reply".to_vec(),
            vec![call("a"), call("b")],
        )
        .unwrap();
    store
        .provider_command(
            &reserved,
            Command::Complete {
                identity: id.clone(),
                call: "a".into(),
                ok: true,
                result: "kept".into(),
            },
        )
        .unwrap();
    let (fresh, new) = reclaim(&root, &store);
    let recovered = fresh.recover_provider_turns(&new).unwrap();
    assert_eq!(recovered.rounds.len(), 1);
    assert_eq!(recovered.rounds[0].calls[0].result, "kept");
    assert!(recovered.rounds[0].calls[0].ok);
    assert!(!recovered.rounds[0].calls[1].ok);
    assert!(recovered.rounds[0].calls[1]
        .result
        .contains("indeterminate"));
    let turns = reopen(&root, &fresh).snapshot().unwrap().provider_turns;
    assert_eq!(
        turns[0].executions[0].completion,
        Some((true, "kept".into()))
    );
    assert!(turns[0].executions[1].completion.is_some());
    assert!(store
        .provider_command(
            &reserved,
            Command::Complete {
                identity: id,
                call: "b".into(),
                ok: true,
                result: "late".into()
            }
        )
        .is_err());
}
#[test]
fn late_reply_suppressed_under_strictly_newer_epoch() {
    let (root, store, reserved) = fixture();
    let id = store
        .provider_intent(&reserved, intent(&reserved, TurnKind::Conversation))
        .unwrap()
        .consume()
        .identity;
    let epoch = store.snapshot().unwrap().provider_epoch;
    let (fresh, new) = reclaim(&root, &store);
    assert!(fresh.snapshot().unwrap().provider_epoch > epoch);
    assert!(store
        .admit_provider_response(&reserved, &id, b"late".to_vec(), vec![call("bad")])
        .is_err());
    let record = fresh.snapshot().unwrap().provider_turns.remove(0);
    assert!(record.response.is_none());
    assert!(record.executions.is_empty());
    assert!(
        fresh
            .provider_intent(&new, intent(&new, TurnKind::Conversation))
            .unwrap()
            .intent()
            .epoch
            > epoch
    );
}
#[test]
fn identity_bounds_and_cross_branch_commands_fail_closed() {
    let (_root, store, reserved) = fixture();
    let mut send = intent(&reserved, TurnKind::Management);
    send.allowance = 2;
    let id = store
        .provider_intent(&reserved, send.clone())
        .unwrap()
        .consume()
        .identity;
    assert!(store.provider_intent(&reserved, send).is_err());
    assert!(store
        .provider_command(
            &reserved,
            Command::Stage {
                identity: id.clone(),
                bytes: vec![0; 3]
            }
        )
        .is_err());
    assert_eq!(
        store.snapshot().unwrap().provider_turns[0].status,
        Status::Intent
    );
    assert!(store
        .provider_command(
            &reserved,
            Command::Recover {
                branch: "other".into()
            }
        )
        .is_err());
    store
        .provider_command(
            &reserved,
            Command::Stage {
                identity: id.clone(),
                bytes: vec![0; 2],
            },
        )
        .unwrap();
    assert!(store
        .provider_command(
            &reserved,
            Command::Admit {
                identity: id,
                executions: vec![call("a"), call("a")]
            }
        )
        .is_err());
    assert_eq!(
        store.snapshot().unwrap().provider_turns[0].status,
        Status::Pending
    );
}
