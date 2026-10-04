//! Deterministic independent F1/F2 regressions; no wall-clock trial loop.
use super::effective_compaction::read_reply;
use super::*;
use crate::agent::over_limit::request_total_bytes;
use serdes_ai::core::{ModelRequest, ModelRequestPart};

fn timestamps(requests: &mut [ModelRequest], stamp: &str) {
    for part in requests.iter_mut().flat_map(|r| &mut r.parts) {
        match part {
            ModelRequestPart::SystemPrompt(p) => p.timestamp = stamp.parse().unwrap(),
            ModelRequestPart::UserPrompt(p) => p.timestamp = stamp.parse().unwrap(),
            ModelRequestPart::ToolReturn(p) => p.timestamp = stamp.parse().unwrap(),
            ModelRequestPart::RetryPrompt(p) => p.timestamp = stamp.parse().unwrap(),
            ModelRequestPart::BuiltinToolReturn(p) => p.timestamp = stamp.parse().unwrap(),
            ModelRequestPart::ModelResponse(p) => p.timestamp = stamp.parse().unwrap(),
        }
    }
}

#[test]
fn timestamps_alone_are_never_content_recovery() {
    let mut full = vec![user_request("mandatory"), assistant_request(&stop_reply())];
    timestamps(&mut full, "2026-10-04T03:12:08.269228001Z");
    let mut short = full.clone();
    timestamps(&mut short, "2026-10-04T03:12:08Z");
    assert!(estimate_request_bytes(&short) < estimate_request_bytes(&full));
    assert!(!over_limit::content::content_shrank(&full, &short));
    assert!(!over_limit::content::content_shrank(&short, &full));
}

#[test]
fn mandatory_history_rejection_calls_backend_exactly_once() {
    let (cwd, store, mut reserved) = recovery_store("mandatory-history-rejection");
    for i in 0..100 {
        reserved.history.push(crate::session::HistoryTurn {
            turn: i,
            attempt: 1,
            branch_id: format!("h{i}"),
            prompt: "prior routing".into(),
            rounds: vec![],
            summary: "completed summary".into(),
        });
    }
    let backend = std::sync::Arc::new(RecoveryBackend::new(vec![Err(
        "context length exceeded".into()
    )]));
    let agent = CodingAgent::with_backend(Box::new(backend), cwd.path().into(), false);
    let turn = Turn::new(&agent).unwrap();
    let mut requests = agent.materialize_requests(&reserved);
    timestamps(&mut requests, "2026-10-04T03:12:08.269228001Z");
    let before = requests.clone();
    assert!(!turn.compact_provider_context(&mut reserved, &mut requests, &[]));
    assert_eq!(requests, before, "unaffected creation metadata stays exact");
    let error = turn
        .first_completion(
            &store,
            &mut reserved,
            &mut requests,
            &crate::tools::tool_specs(false),
        )
        .unwrap_err();
    assert_eq!(error.key, "context-limit");
    assert!(error.message.contains("no retry sent"));
    assert_eq!(agent.model_calls(), 1);
}

#[test]
fn genuine_history_shrink_retains_unaffected_metadata_and_is_idempotent() {
    let (cwd, _store, mut reserved) = recovery_store("history-metadata");
    reserved.history.push(compactable_history());
    let agent = CodingAgent::with_backend(
        Box::new(RecoveryBackend::new(vec![])),
        cwd.path().into(),
        false,
    );
    let turn = Turn::new(&agent).unwrap();
    let mut requests = agent.materialize_requests(&reserved);
    timestamps(&mut requests, "2026-10-04T03:12:08.269228001Z");
    let before = requests.clone();
    assert!(turn.compact_provider_context(&mut reserved, &mut requests, &[]));
    assert_eq!(requests[0], before[0]);
    assert_eq!(requests[1], before[1]);
    assert_eq!(requests.last(), before.last());
    let reduced = requests.clone();
    assert!(!turn.compact_provider_context(&mut reserved, &mut requests, &[]));
    assert_eq!(requests, reduced);
}

#[test]
fn forced_provider_rejection_below_local_guard_recovers_real_admitted_reads() {
    let (cwd, store, reserved) = recovery_store("forced-provider-finding");
    std::fs::write(
        cwd.path().join("evidence.txt"),
        "evidence line\n".repeat(1000),
    )
    .unwrap();
    let mut empty = stop_reply();
    empty.text.clear();
    let backend = std::sync::Arc::new(RecoveryBackend::new(vec![
        Ok(read_reply("r1")),
        Ok(read_reply("r2")),
        Ok(read_reply("r3")),
        Ok(empty),
        Err("context length exceeded".into()),
        Ok(stop_reply()),
    ]));
    let agent = CodingAgent::with_backend(Box::new(backend.clone()), cwd.path().into(), false);
    assert_eq!(agent.run(&store, &reserved).unwrap().summary, "done");
    assert_eq!(agent.model_calls(), 6);
    let captured = backend.requests.lock().unwrap();
    let tools = crate::tools::tool_specs(false);
    assert!(request_total_bytes(&captured[4], &tools) < materialization_budget(None));
    assert!(
        request_total_bytes(&captured[5], &tools)
            < request_total_bytes(&captured[4], &tools) - 20_000
    );
    assert_eq!(
        captured[4].last(),
        captured[5].last(),
        "final instruction, including metadata, stays exact"
    );
    let returns: Vec<_> = captured[5].iter().flat_map(|r| r.tool_returns()).collect();
    assert_eq!(returns.len(), 3);
    assert!(returns[0]
        .content
        .as_text()
        .unwrap()
        .contains("CTXDIGEST v1"));
    assert!(returns[1]
        .content
        .as_text()
        .unwrap()
        .contains("CTXDIGEST v1"));
    assert_eq!(
        returns[2],
        captured[4]
            .iter()
            .flat_map(|r| r.tool_returns())
            .last()
            .unwrap()
    );
    assert_eq!(
        store.snapshot().unwrap().branches[0].lifecycle,
        crate::session::Lifecycle::Completed
    );
}

#[test]
fn forced_provider_rejection_without_eligible_content_does_not_replay() {
    let (cwd, store, reserved) = recovery_store("forced-unchanged-finding");
    let mut empty = stop_reply();
    empty.text.clear();
    let agent = CodingAgent::with_backend(
        Box::new(RecoveryBackend::new(vec![
            Ok(empty),
            Err("context length exceeded".into()),
        ])),
        cwd.path().into(),
        false,
    );
    let error = agent.run(&store, &reserved).unwrap_err();
    assert_eq!(error.key, "context-limit");
    assert!(error.message.contains("no retry sent"));
    assert_eq!(agent.model_calls(), 2);
}
