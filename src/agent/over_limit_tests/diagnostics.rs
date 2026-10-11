//! Measured one-shot context recovery diagnostics and retained-size accounting.

use super::*;
use crate::agent::over_limit::request_total_bytes;

fn reported_bytes(message: &str, prefix: &str, suffix: &str) -> usize {
    message
        .split_once(prefix)
        .expect("diagnostic must name the measured quantity")
        .1
        .split_once(suffix)
        .expect("diagnostic must name the byte unit")
        .0
        .parse()
        .expect("diagnostic must report an integer byte count")
}

// Materialization creates timestamps whose serialized lengths can vary. Read the
// agent's own totals, then check every excess/reduction against those exact values.
fn reported_estimates(message: &str) -> (usize, usize) {
    (
        reported_bytes(message, "estimated request would be ", " bytes"),
        reported_bytes(message, "after one compaction attempt, still ", " bytes"),
    )
}

fn assert_measurements(message: &str, before: usize, after: usize, budget: usize) {
    assert!(before >= after);
    assert!(after > budget);
    assert!(message.contains(&format!("estimated request would be {before} bytes")));
    assert!(message.contains(&format!(
        "{} bytes over the {budget}-byte heuristic guard",
        before - budget
    )));
    assert!(message.contains(&format!(
        "still {after} bytes ({} bytes over guard)",
        after - budget
    )));
    assert!(message.contains(&format!("reduced by {} bytes", before - after)));
    assert!(message.contains("retained estimated bytes: system/instructions="));
    assert!(message.contains("prompts/notices="));
    assert!(message.contains("tool results="));
    assert!(message.contains("tool schemas="));
    assert!(message.contains("no request sent"));
    let retained: usize = [
        "system/instructions=",
        "prompts/notices=",
        "tool results=",
        "assistant/call framing=",
        "fixed/request/part overhead=",
        "tool schemas=",
    ]
    .iter()
    .map(|prefix| {
        reported_bytes(
            message,
            prefix,
            if *prefix == "tool schemas=" { ";" } else { "," },
        )
    })
    .sum();
    assert_eq!(retained, after);
}

#[test]
fn measurement_assertions_reject_inconsistent_reported_numbers() {
    let message = "estimated request would be 40000 bytes (10000 bytes over the 30000-byte heuristic guard); after one compaction attempt, still 35000 bytes (5000 bytes over guard); reduced by 5000 bytes; retained estimated bytes: system/instructions=1000, prompts/notices=30000, tool results=0, assistant/call framing=1000, fixed/request/part overhead=1000, tool schemas=2000; no request sent";
    let check = |message: &str| {
        let (before, after) = reported_estimates(message);
        assert_measurements(message, before, after, 30_000);
    };
    check(message);
    for (old, new) in [
        ("would be 40000", "would be 40001"),
        ("still 35000", "still 35001"),
        ("(10000 bytes over", "(10001 bytes over"),
        ("(5000 bytes over", "(5001 bytes over"),
        ("reduced by 5000", "reduced by 4999"),
        ("tool schemas=2000", "tool schemas=2001"),
    ] {
        let corrupted = message.replace(old, new);
        assert_ne!(corrupted, message);
        assert!(std::panic::catch_unwind(|| check(&corrupted)).is_err());
    }
}

#[test]
fn no_eligible_history_reports_actual_excess_and_retained_prompt() {
    let prompt = "界".repeat(25_000);
    let (cwd, store, reserved) = recovery_store_with_prompt("diagnostic-prompt", &prompt);
    let agent = CodingAgent::with_backend(
        Box::new(RecoveryBackend::new(vec![])),
        cwd.path().into(),
        false,
    )
    .with_context_limit(Some(10_000));
    let error = agent.run(&store, &reserved).unwrap_err();
    let (before, after) = reported_estimates(&error.message);
    assert_eq!(before, after);
    assert!(after > prompt.len());
    assert_eq!(error.key, "context-limit");
    assert_measurements(&error.message, before, after, 30_000);
    assert!(error
        .message
        .contains("no eligible history or observed tool results could be removed"));
    assert!(!error.message.contains(&prompt));
    assert_eq!(agent.model_calls(), 0);
}

#[test]
fn reduced_but_oversized_preflight_names_both_estimates() {
    let (cwd, store, mut reserved) =
        recovery_store_with_prompt("diagnostic-reduced", &"x".repeat(60_000));
    reserved.history.push(compactable_history());
    let agent = CodingAgent::with_backend(
        Box::new(RecoveryBackend::new(vec![])),
        cwd.path().into(),
        false,
    )
    .with_context_limit(Some(10_000));
    let turn = Turn::new(&agent).unwrap();
    let error = turn.preflight_recovery(&store, &mut reserved).unwrap_err();
    let (before, after) = reported_estimates(&error.message);
    assert!(after < before);
    assert!(after > reserved.prompt.len());
    assert_eq!(error.key, "context-limit");
    assert_measurements(&error.message, before, after, 30_000);
    assert!(error
        .message
        .contains("compaction reduced the request but retained content still exceeds the guard"));
    assert!(!error.message.contains("no eligible history"));
    assert_eq!(agent.model_calls(), 0);
}

#[test]
fn reduced_but_oversized_live_request_keeps_latest_result_and_checkpoint() {
    let (cwd, store, mut reserved) = recovery_store("diagnostic-live");
    let agent = CodingAgent::with_backend(
        Box::new(RecoveryBackend::new(vec![])),
        cwd.path().into(),
        false,
    )
    .with_context_limit(Some(10_000));
    let old = boundaries::admitted_round(&store, "observed", &"old evidence\n".repeat(1200));
    let latest = boundaries::admitted_round(&store, "latest", &"界".repeat(15_000));
    let rounds = vec![old, latest];
    store.checkpoint(&reserved, &rounds).unwrap();
    let mut requests = agent.materialize_requests(&reserved);
    for round in &rounds {
        boundaries::append_live(&mut requests, round);
    }
    let tools = crate::tools::tool_specs(false);
    let before = request_total_bytes(&requests, &tools);
    let error = Turn::new(&agent)
        .unwrap()
        .recover_request_budget(&store, &mut reserved, &mut requests, &tools, &rounds)
        .unwrap_err();
    let after = request_total_bytes(&requests, &tools);
    assert!(after < before);
    assert_measurements(&error.message, before, after, 30_000);
    assert!(error
        .message
        .contains("compaction reduced the request but retained content still exceeds the guard"));
    let returns: Vec<_> = requests.iter().flat_map(|r| r.tool_returns()).collect();
    assert_eq!(
        returns[0].content.as_text(),
        Some(rounds[0].calls[0].result.as_str())
    );
    assert_eq!(
        returns[1].content.as_text(),
        Some(rounds[1].calls[0].result_live.as_str())
    );
    assert_eq!(agent.model_calls(), 0);
    let state = store.snapshot().unwrap();
    assert_eq!(
        state.branches[0].lifecycle,
        crate::session::Lifecycle::Failed
    );
    assert_eq!(state.branches[0].rounds.len(), 2);
    assert_eq!(state.branches[0].rounds[1].calls[0].id, "latest");
}
