//! Assistant-dominated long-turn regressions for issue #388.
use super::*;
use crate::agent::over_limit::request_total_bytes;
use serdes_ai::core::{ModelRequestPart, ModelResponsePart};

fn large_read(id: &str) -> LlmResult {
    let mut reply = super::effective_compaction::read_reply(id);
    reply.text = format!("Older narration {id}: {}", "界".repeat(12000));
    reply
}

#[test]
fn assistant_dominated_long_turn_compacts_under_guard_and_continues() {
    let (cwd, store, reserved) = recovery_store("assistant-long-turn");
    std::fs::write(cwd.path().join("evidence.txt"), "latest evidence bytes").unwrap();
    let mut replies = (0..24)
        .map(|i| Ok(large_read(&format!("long-{i}"))))
        .collect::<Vec<_>>();
    replies.push(Ok(stop_reply()));
    let backend = std::sync::Arc::new(RecoveryBackend::new(replies));
    let agent = CodingAgent::with_backend(Box::new(backend.clone()), cwd.path().into(), false)
        .with_context_limit(Some(262_144));
    let run = agent
        .run(&store, &reserved)
        .expect("assistant framing must be reclaimable");
    assert_eq!(run.status, "ok");
    assert_eq!(run.tool_count, 24);
    let captured = backend.requests.lock().unwrap();
    assert_eq!(captured.len(), 25);
    let tools = crate::tools::tool_specs(false);
    for requests in captured.iter() {
        assert!(request_total_bytes(requests, &tools) <= 786_432);
        crate::context_kernel::security::legal_quotes(requests).unwrap();
    }
    let last = captured.last().unwrap();
    assert!(last.iter().flat_map(|r| r.tool_returns()).count() < 24);
    let json = serde_json::to_string(last).unwrap();
    assert!(json.contains("current prompt verbatim"));
    assert!(json.contains("long-23"));
    assert!(json.contains("latest evidence bytes"));
    assert_eq!(store.snapshot().unwrap().branches[0].rounds.len(), 25);
    let last = last.clone();
    drop(captured);
    drop(store);
    let reopened = SessionStore::load(&SessionId::parse("assistant-long-turn").unwrap()).unwrap();
    let next = reopened
        .start_request(None, None, "next turn", cwd.path())
        .unwrap();
    let resumed = std::sync::Arc::new(RecoveryBackend::new(vec![Ok(stop_reply())]));
    CodingAgent::with_backend(Box::new(resumed.clone()), cwd.path().into(), false)
        .with_context_limit(Some(262_144))
        .run(&reopened, &next)
        .unwrap();
    assert!(resumed.requests.lock().unwrap()[0].starts_with(&last));
}

#[test]
fn signed_older_reasoning_is_elided_as_a_whole_pair_not_rewritten() {
    let (cwd, _store, mut reserved) = recovery_store("assistant-reasoning");
    let agent = CodingAgent::with_backend(
        Box::new(RecoveryBackend::new(vec![])),
        cwd.path().into(),
        false,
    )
    .with_context_limit(Some(20_000));
    let turn = Turn::new(&agent).unwrap();
    let mut requests = agent.materialize_requests(&reserved);
    for id in ["old-reasoning", "latest-reasoning"] {
        let mut response = match assistant_request(&large_read(id)).parts.remove(0) {
            ModelRequestPart::ModelResponse(response) => response,
            _ => unreachable!(),
        };
        let mut thinking = serdes_ai::core::ThinkingPart::new("private reasoning".repeat(1000));
        thinking.signature = Some(format!("signature-{id}"));
        response
            .parts
            .insert(0, ModelResponsePart::Thinking(thinking));
        requests.push(serdes_ai::core::ModelRequest {
            parts: vec![ModelRequestPart::ModelResponse(response)],
            ..Default::default()
        });
        requests.push(tool_return_request("read_file", id, true, "evidence"));
    }
    requests.push(user_request("mandatory correction"));
    let latest = requests[requests.len() - 3..].to_vec();
    assert!(turn.compact_provider_context(&mut reserved, &mut requests, &[]));
    assert!(requests.ends_with(&latest));
    let json = serde_json::to_string(&requests).unwrap();
    assert!(!json.contains("old-reasoning"));
    assert!(json.contains("signature-latest-reasoning"));
    assert!(!turn.compact_provider_context(&mut reserved, &mut requests, &[]));
}
