//! Pairing and oldest-first pressure boundaries for assistant reclamation.
use super::*;
use crate::agent::over_limit::request_total_bytes;

fn argument_reply(index: usize) -> LlmResult {
    let mut reply = stop_reply();
    reply.text = String::new();
    reply.calls = (0..2)
        .map(|call| ToolCall {
            id: format!("batch-{index}-{call}"),
            name: "write_file".into(),
            args_json: serde_json::json!({"path":"output.txt","content":"界".repeat(3000)})
                .to_string(),
        })
        .collect();
    reply
}

fn append_batch(requests: &mut Vec<serdes_ai::core::ModelRequest>, reply: &LlmResult) {
    requests.push(assistant_request(reply));
    for call in &reply.calls {
        requests.push(tool_return_request(&call.name, &call.id, true, "written"));
    }
}

#[test]
fn one_recovery_keeps_reducing_oldest_batches_until_the_complete_guard_fits() {
    let (cwd, _store, mut reserved) = recovery_store("assistant-batches");
    reserved.history.push(compactable_history());
    let agent = CodingAgent::with_backend(
        Box::new(RecoveryBackend::new(vec![])),
        cwd.path().into(),
        false,
    )
    .with_context_limit(Some(20_000));
    let turn = Turn::new(&agent).unwrap();
    let mut requests = agent.materialize_requests(&reserved);
    for index in 0..6 {
        append_batch(&mut requests, &argument_reply(index));
    }
    requests.push(user_request("mandatory correction"));
    let latest = requests[requests.len() - 4..].to_vec();
    let tools = crate::tools::tool_specs(false);
    assert!(request_total_bytes(&requests, &tools) > 60_000);
    assert!(turn.compact_provider_context(&mut reserved, &mut requests, &[]));
    assert!(request_total_bytes(&requests, &tools) <= 60_000);
    assert!(requests.ends_with(&latest));
    let json = serde_json::to_string(&requests).unwrap();
    for index in 0..4 {
        assert!(!json.contains(&format!("batch-{index}-")));
    }
    for index in 4..6 {
        assert!(json.contains(&format!("batch-{index}-")));
    }
    assert!(json.contains("Full completed summary retained"));
    assert!(json.contains("prior prompt"));
    assert!(json.contains("current prompt verbatim"));
    assert!(json.contains("mandatory correction"));
    assert_eq!(requests.iter().flat_map(|r| r.tool_returns()).count(), 4);
}

#[test]
fn malformed_or_ambiguous_pairs_are_not_partially_elided() {
    for defect in [
        "missing",
        "duplicate-return",
        "duplicate-call",
        "wrong-name",
        "mixed-return",
    ] {
        let (cwd, _store, mut reserved) = recovery_store(&format!("assistant-{defect}"));
        let agent = CodingAgent::with_backend(
            Box::new(RecoveryBackend::new(vec![])),
            cwd.path().into(),
            false,
        )
        .with_context_limit(Some(10_000));
        let turn = Turn::new(&agent).unwrap();
        let mut requests = agent.materialize_requests(&reserved);
        let start = requests.len();
        append_batch(&mut requests, &argument_reply(0));
        match defect {
            "missing" => {
                requests.pop();
            }
            "duplicate-return" => requests.push(requests.last().unwrap().clone()),
            "duplicate-call" => requests.push(requests[start].clone()),
            "wrong-name" => {
                requests[start + 1] = tool_return_request("read_file", "batch-0-0", true, "written")
            }
            "mixed-return" => requests[start + 1]
                .parts
                .extend(user_request("mandatory mixed prompt").parts),
            _ => unreachable!(),
        }
        append_batch(&mut requests, &argument_reply(1));
        let before = requests.clone();
        assert!(
            !turn.compact_provider_context(&mut reserved, &mut requests, &[]),
            "{defect}"
        );
        assert_eq!(requests, before, "{defect}");
    }
}
