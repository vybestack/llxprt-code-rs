//! Recovery must not change accounting, caps, or final-summary live boundaries.
use super::effective_compaction::read_reply;
use super::*;

#[test]
fn forced_summary_reclaims_observed_results_without_releasing_latest() {
    let (cwd, store, reserved) = recovery_store("compacted-forced-summary");
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
        Ok(stop_reply()),
    ]));
    let agent =
        CodingAgent::with_backend(Box::new(backend.clone()), cwd.path().to_path_buf(), false)
            .with_context_limit(Some(16_000));
    assert_eq!(agent.run(&store, &reserved).unwrap().tool_count, 3);
    let requests = backend.requests.lock().unwrap();
    let final_request = requests.last().unwrap();
    let returns: Vec<_> = final_request
        .iter()
        .flat_map(|r| r.tool_returns())
        .collect();
    assert!(returns[0]
        .content
        .as_text()
        .unwrap()
        .contains("CTXDIGEST v1"));
    assert!(returns[2]
        .content
        .as_text()
        .unwrap()
        .contains("evidence line"));
    assert!(serde_json::to_string(final_request)
        .unwrap()
        .contains("Provide your final plain-text summary"));
}

#[test]
fn compaction_does_not_refund_live_output_usage_under_backpressure() {
    let (cwd, store, reserved) = recovery_store("compaction-output-backpressure");
    std::fs::write(
        cwd.path().join("evidence.txt"),
        "evidence line\n".repeat(1000),
    )
    .unwrap();
    let backend = std::sync::Arc::new(RecoveryBackend::new(vec![
        Ok(read_reply("r1")),
        Ok(read_reply("r2")),
        Ok(read_reply("r3")),
        Ok(read_reply("r4")),
        Ok(stop_reply()),
    ]));
    let agent =
        CodingAgent::with_backend(Box::new(backend.clone()), cwd.path().to_path_buf(), false)
            .with_context_limit(Some(16_000))
            .with_output_caps(OutputCaps {
                shell: 16_000,
                tool: 16_000,
                turn: 45_000,
            });
    let run = agent.run(&store, &reserved).unwrap();
    assert_eq!(run.tool_count, 4);
    let state = store.snapshot().unwrap();
    let last = &state.branches[0].rounds[3].calls[0];
    // The fourth read has only the original live-byte balance, not space refunded
    // by replacing earlier request results with digest records.
    assert!(last.result.contains("CTXDIGEST v1"));
    let requests = backend.requests.lock().unwrap();
    let latest = requests
        .last()
        .unwrap()
        .iter()
        .flat_map(|r| r.tool_returns())
        .last()
        .unwrap();
    let text = latest.content.as_text().unwrap();
    assert!(text.len() < 8000);
    assert!(text.contains("window clamped"), "{text}");
    assert!(text.contains("by output budget"), "{text}");
    assert!(text.contains("re-fetch:"));
}
