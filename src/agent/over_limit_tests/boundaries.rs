//! Mandatory-boundary and faithful request-projection tests.

use super::*;
use crate::agent::over_limit::request_total_bytes;
use crate::session::{HistoryTurn, ToolCallRecord};

fn admitted_round(store: &SessionStore, id: &str, text: &str) -> RoundRecord {
    let projection = store.compact_tool_result("read_file", text).unwrap();
    RoundRecord {
        assistant: format!("Observed {id}"),
        calls: vec![ToolCallRecord {
            id: id.into(),
            name: "read_file".into(),
            args: "{}".into(),
            ok: true,
            refused: false,
            result: projection.persisted,
            result_live: projection.live,
        }],
    }
}

fn append_live(requests: &mut Vec<serdes_ai::core::ModelRequest>, round: &RoundRecord) {
    let start = requests.len();
    requests.extend(crate::adapter::persisted_round_requests(round));
    for (request, call) in requests[start + 1..].iter_mut().zip(&round.calls) {
        *request =
            crate::adapter::tool_return_request(&call.name, &call.id, call.ok, &call.result_live);
    }
}

#[test]
fn measured_request_shrinks_once_preserving_latest_multibyte_and_pairing() {
    let (cwd, store, mut reserved) = recovery_store("measured-current");
    let agent = CodingAgent::with_backend(
        Box::new(RecoveryBackend::new(vec![])),
        cwd.path().to_path_buf(),
        false,
    );
    let turn = Turn::new(&agent).unwrap();
    let old = admitted_round(&store, "old", &"old evidence\n".repeat(1200));
    let latest = admitted_round(&store, "latest", &"最新 evidence\n".repeat(1200));
    let mut requests = agent.materialize_requests(&reserved);
    append_live(&mut requests, &old);
    append_live(&mut requests, &latest);
    requests.push(user_request("mandatory correction notice"));
    let tools = crate::tools::tool_specs(false);
    let original = request_total_bytes(&requests, &tools);
    let rounds = vec![old, latest];
    assert!(turn.compact_provider_context(&mut reserved, &mut requests, &rounds));
    let after = request_total_bytes(&requests, &tools);
    println!("controlled source request bytes: before={original} after={after}");
    assert!(after < original - 10_000);
    let returns: Vec<_> = requests.iter().flat_map(|r| r.tool_returns()).collect();
    assert_eq!(returns.len(), 2);
    assert_eq!(returns[0].tool_call_id.as_deref(), Some("old"));
    assert_eq!(returns[1].tool_call_id.as_deref(), Some("latest"));
    assert_eq!(
        returns[0].content.as_text(),
        Some(rounds[0].calls[0].result.as_str())
    );
    assert_eq!(
        returns[1].content.as_text(),
        Some(rounds[1].calls[0].result_live.as_str())
    );
    let json = serde_json::to_string(&requests).unwrap();
    assert!(json.contains("mandatory correction notice"));
    assert!(json.contains("current prompt verbatim"));
    assert!(json.contains("CTXDIGEST v1"));
    assert!(!turn.compact_provider_context(&mut reserved, &mut requests, &rounds));
    assert_eq!(request_total_bytes(&requests, &tools), after);
}

#[test]
fn latest_mandatory_mass_fails_without_dropping_new_calls() {
    let (cwd, store, mut reserved) = recovery_store("mandatory-mass");
    let agent = CodingAgent::with_backend(
        Box::new(RecoveryBackend::new(vec![])),
        cwd.path().to_path_buf(),
        false,
    )
    .with_context_limit(Some(10_000));
    let turn = Turn::new(&agent).unwrap();
    let round = admitted_round(&store, "new-call", &"界".repeat(15_000));
    store
        .checkpoint(&reserved, std::slice::from_ref(&round))
        .unwrap();
    let mut requests = agent.materialize_requests(&reserved);
    append_live(&mut requests, &round);
    let before = request_total_bytes(&requests, &crate::tools::tool_specs(false));
    let error = turn
        .recover_request_budget(
            &store,
            &mut reserved,
            &mut requests,
            &crate::tools::tool_specs(false),
            std::slice::from_ref(&round),
        )
        .unwrap_err();
    assert_eq!(error.key, "context-limit");
    assert!(error.message.contains(&format!("still {before} bytes")));
    assert_eq!(agent.model_calls(), 0);
    let result = requests
        .iter()
        .flat_map(|r| r.tool_returns())
        .next()
        .unwrap();
    assert_eq!(
        result.content.as_text(),
        Some(round.calls[0].result_live.as_str())
    );
    let state = store.snapshot().unwrap();
    assert_eq!(
        state.branches[0].lifecycle,
        crate::session::Lifecycle::Failed
    );
    assert_eq!(state.branches[0].rounds[0].calls[0].id, "new-call");
    assert!(state.branches[0].rounds[0].calls[0].result_live.is_empty());
}

#[test]
fn compacted_history_keeps_semantic_summary_and_tool_pairs() {
    let (cwd, store, mut reserved) = recovery_store("semantic-history");
    let round = admitted_round(&store, "prior-call", &"prior evidence\n".repeat(100));
    reserved.history.push(HistoryTurn {
        turn: 0,
        attempt: 1,
        branch_id: "prior".into(),
        prompt: "prior routing".into(),
        rounds: vec![round],
        summary: "Completed task and test status verbatim".into(),
    });
    let agent = CodingAgent::with_backend(
        Box::new(RecoveryBackend::new(vec![])),
        cwd.path().to_path_buf(),
        false,
    );
    assert!(crate::agent::over_limit::compact_history(
        &mut reserved.history
    ));
    let requests = agent.materialize_requests(&reserved);
    let json = serde_json::to_string(&requests).unwrap();
    assert!(json.contains("Completed task and test status verbatim"));
    assert!(json.contains("prior routing"));
    assert!(json.contains("prior-call"));
    assert!(json.contains("CTXDIGEST v1"));
    assert_eq!(requests.iter().flat_map(|r| r.tool_returns()).count(), 1);
}

#[test]
fn multibyte_prompt_and_system_mass_are_noncompactable() {
    let mass = "界".repeat(25_000);
    for system in [false, true] {
        let (cwd, store, reserved) = recovery_store_with_prompt(
            if system { "system-mass" } else { "prompt-mass" },
            if system {
                "mandatory prompt"
            } else {
                mass.as_str()
            },
        );
        let mut agent = CodingAgent::with_backend(
            Box::new(RecoveryBackend::new(vec![])),
            cwd.path().to_path_buf(),
            false,
        )
        .with_context_limit(Some(10_000));
        if system {
            agent.prompt_notes = Some("界".repeat(25_000));
        }
        let original = request_total_bytes(
            &agent.materialize_requests(&reserved),
            &crate::tools::tool_specs(false),
        );
        let error = agent.run(&store, &reserved).unwrap_err();
        assert_eq!(error.key, "context-limit");
        assert!(error.message.contains(&format!("still {original} bytes")));
        assert_eq!(agent.model_calls(), 0);
    }
}
