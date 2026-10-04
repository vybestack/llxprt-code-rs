//! Registered-tool reproduction of the unchanged current-turn compaction guard.

use super::*;
use crate::agent::over_limit::request_total_bytes;

pub(super) fn read_reply(id: &str) -> LlmResult {
    LlmResult {
        text: format!("Reading evidence {id}"),
        calls: vec![ToolCall {
            id: id.into(),
            name: "read_file".into(),
            args_json: r#"{"path":"evidence.txt","max_output_bytes":16000}"#.into(),
        }],
        finish_reason: Some(FinishReason::ToolCall),
        usage: LlmUsage::default(),
    }
}

#[test]
fn current_tool_history_compacts_at_the_owning_request_source() {
    let (cwd, store, reserved) = recovery_store("effective-current");
    std::fs::write(
        cwd.path().join("evidence.txt"),
        "evidence line\n".repeat(1000),
    )
    .unwrap();
    let backend = std::sync::Arc::new(RecoveryBackend::new(vec![
        Ok(read_reply("r1")),
        Ok(read_reply("r2")),
        Ok(read_reply("r3")),
        Ok(stop_reply()),
    ]));
    let agent =
        CodingAgent::with_backend(Box::new(backend.clone()), cwd.path().to_path_buf(), false)
            .with_context_limit(Some(16_000));
    let run = agent
        .run(&store, &reserved)
        .expect("old results have admitted digests");
    assert_eq!(run.tool_count, 3);
    let captured = backend.requests.lock().unwrap();
    assert_eq!(captured.len(), 4);
    let tools = crate::tools::tool_specs(false);
    for requests in captured.iter() {
        assert!(request_total_bytes(requests, &tools) <= 48_000);
    }
    let last = serde_json::to_string(captured.last().unwrap()).unwrap();
    assert!(last.contains("CTXDIGEST v1"));
    assert!(last.contains("current prompt verbatim"));
    assert!(last.contains("evidence line"));
    let state = store.snapshot().unwrap();
    let branch = &state.branches[0];
    assert_eq!(branch.lifecycle, crate::session::Lifecycle::Completed);
    for (index, round) in branch.rounds[..3].iter().enumerate() {
        assert_eq!(round.calls[0].id, format!("r{}", index + 1));
        assert!(round.calls[0].result.contains("CTXDIGEST v1"));
        assert!(round.calls[0].result_live.is_empty());
    }
}
