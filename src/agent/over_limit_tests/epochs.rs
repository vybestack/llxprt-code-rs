//! Epoch-cache acceptance through the production send/store path.
use super::*;
use crate::context_policy::cache::CutTrigger;
use serdes_ai::core::ModelRequest;

fn epochs(store: &SessionStore) -> crate::context_policy::cache::EpochCache {
    store
        .context
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .policy
        .epoch_cache()
        .clone()
}

#[test]
fn sent_prefix_cannot_change_without_a_checkpoint_and_survives_restart() {
    let (cwd, store, reserved) = recovery_store("epoch-prefix-restart");
    let agent = CodingAgent::with_backend(
        Box::new(RecoveryBackend::new(vec![])),
        cwd.path().into(),
        false,
    );
    let original = agent.materialize_requests(&reserved);
    let epoch = store
        .record_request_prefix(&reserved, &original, None)
        .unwrap();
    let mut appended = original.clone();
    appended.push(crate::adapter::user_request("append"));
    assert_eq!(
        store
            .record_request_prefix(&reserved, &appended, None)
            .unwrap(),
        epoch
    );
    let before = epochs(&store);
    assert_eq!(before.records().len(), 1);
    assert_eq!(before.records()[0].invalidations, 0);
    assert!(store
        .record_request_prefix(&reserved, &original, None)
        .unwrap_err()
        .to_string()
        .contains("without a checkpoint"));
    assert_eq!(before, epochs(&store));
    drop(store);
    let reopened = SessionStore::load(&SessionId::parse("epoch-prefix-restart").unwrap()).unwrap();
    assert_eq!(
        reopened
            .record_request_prefix(&reserved, &appended, None)
            .unwrap(),
        epoch
    );
    assert_eq!(before, epochs(&reopened));
    assert!(reopened
        .record_request_prefix(&reserved, &original, None)
        .is_err());
    assert_eq!(
        reopened
            .record_request_prefix(&reserved, &original, Some(CutTrigger::ExplicitRequest))
            .unwrap(),
        epoch + 1
    );
    let after = epochs(&reopened);
    assert_eq!(after.records()[1].invalidations, 1);
    assert_eq!(
        after.records()[1].trigger,
        Some(CutTrigger::ExplicitRequest)
    );
}

#[test]
fn actual_pressure_cut_is_one_invalidation_then_only_appends_across_turns() {
    let (cwd, store, reserved) = recovery_store("epoch-live-cut");
    std::fs::write(
        cwd.path().join("evidence.txt"),
        "evidence line\n".repeat(1000),
    )
    .unwrap();
    let backend = std::sync::Arc::new(RecoveryBackend::new(vec![
        Ok(super::effective_compaction::read_reply("e1")),
        Ok(super::effective_compaction::read_reply("e2")),
        Ok(super::effective_compaction::read_reply("e3")),
        Ok(stop_reply()),
    ]));
    let agent = CodingAgent::with_backend(Box::new(backend.clone()), cwd.path().into(), false)
        .with_context_limit(Some(16_000));
    agent.run(&store, &reserved).unwrap();
    let before = epochs(&store);
    assert_eq!(before.records().len(), 2);
    assert_eq!(before.records()[0].invalidations, 0);
    assert_eq!(before.records()[1].invalidations, 1);
    assert_eq!(
        before.records()[1].trigger,
        Some(CutTrigger::BudgetPressure)
    );
    assert!(before.records()[1].bytes_reclaimed > 0);
    let last: Vec<ModelRequest> = backend.requests.lock().unwrap().last().unwrap().clone();
    assert!(serde_json::to_string(&last)
        .unwrap()
        .contains("context-checkpoint"));
    drop(store);
    let reopened = SessionStore::load(&SessionId::parse("epoch-live-cut").unwrap()).unwrap();
    let next = reopened
        .start_request(None, None, "next turn", cwd.path())
        .unwrap();
    let next_backend = std::sync::Arc::new(RecoveryBackend::new(vec![Ok(stop_reply())]));
    CodingAgent::with_backend(Box::new(next_backend.clone()), cwd.path().into(), false)
        .run(&reopened, &next)
        .unwrap();
    let sent = next_backend.requests.lock().unwrap();
    assert!(
        sent[0].starts_with(&last),
        "checkpoint head must be restored, not reconstructed"
    );
    let after = epochs(&reopened);
    assert_eq!(after.records().len(), 2);
    assert_eq!(
        before.records()[1].head_digest,
        after.records()[1].head_digest
    );
    assert_eq!(after.records()[1].invalidations, 1);
}

#[test]
fn explicit_request_uses_existing_compactor_but_idle_turn_does_not_cut() {
    let (cwd, store, mut reserved) = recovery_store("epoch-explicit");
    reserved.history.push(super::compactable_history());
    let backend = std::sync::Arc::new(RecoveryBackend::new(vec![Ok(stop_reply())]));
    CodingAgent::with_backend(Box::new(backend), cwd.path().into(), false)
        .with_checkpoint_request()
        .run(&store, &reserved)
        .unwrap();
    let cache = epochs(&store);
    assert_eq!(cache.records().len(), 1);
    assert_eq!(
        cache.records()[0].trigger,
        Some(CutTrigger::ExplicitRequest)
    );
    assert_eq!(cache.records()[0].invalidations, 1);
    let (cwd, store, reserved) = recovery_store("epoch-explicit-idle");
    CodingAgent::with_backend(
        Box::new(RecoveryBackend::new(vec![Ok(stop_reply())])),
        cwd.path().into(),
        false,
    )
    .with_checkpoint_request()
    .run(&store, &reserved)
    .unwrap();
    assert_eq!(epochs(&store).records()[0].invalidations, 0);
}

#[test]
fn missing_or_corrupt_epoch_artifact_is_a_recovery_error_not_a_reset() {
    for name in ["epochs.json", "request-prefixes.json"] {
        let (cwd, store, reserved) =
            recovery_store(&format!("epoch-corrupt-{}", name.replace('.', "-")));
        let agent = CodingAgent::with_backend(
            Box::new(RecoveryBackend::new(vec![Ok(stop_reply())])),
            cwd.path().into(),
            false,
        );
        agent.run(&store, &reserved).unwrap();
        let path = store.session_dir.join("context/committed").join(name);
        std::fs::write(&path, b"not json").unwrap();
        let id = store.session_id.clone();
        drop(store);
        let reopened = SessionStore::load(&SessionId::parse(&id).unwrap()).unwrap();
        let next = reopened
            .start_request(None, None, "next", cwd.path())
            .unwrap();
        let before = std::fs::read(&path).unwrap();
        let error = agent.run(&reopened, &next).unwrap_err();
        assert_eq!(error.key, "session");
        assert!(error.message.contains("corrupt"));
        assert_eq!(before, std::fs::read(path).unwrap());
    }
}
