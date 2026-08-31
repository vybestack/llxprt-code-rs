use super::*;

fn valid_state() -> SessionState {
    SessionState {
        version: STORE_VERSION,
        session_id: "session".to_string(),
        cwd: None,
        cwd_dev: 0,
        cwd_ino: 0,
        branches: vec![BranchRecord {
            branch_id: "b1".to_string(),
            turn: 1,
            attempt: 1,
            parent_branch: None,
            parent_turn: 0,
            parent_attempt: 0,
            prompt: "prompt".to_string(),
            digest: crate::agent::prompt_digest("prompt"),
            lifecycle: Lifecycle::Completed,
            rounds: vec![
                RoundRecord {
                    assistant: String::new(),
                    calls: vec![ToolCallRecord {
                        id: "call-1".to_string(),
                        name: "read_file".to_string(),
                        args: "{}".to_string(),
                        ok: true,
                        result: String::new(),
                        refused: false,
                    }],
                },
                RoundRecord {
                    assistant: "done".to_string(),
                    calls: Vec::new(),
                },
            ],
            summary: "done".to_string(),
            error: String::new(),
            owner: String::new(),
            reserved_at: 0,
            lease_expiry: 0,
        }],
        next_branch_seq: 1,
    }
}

fn corruption_message(state: &SessionState) -> String {
    match state.validate() {
        Err(StoreError::Corrupt(message)) => message,
        other => panic!("expected corrupt state, got {other:?}"),
    }
}

#[test]
fn persisted_tool_call_id_and_name_caps_are_enforced() {
    let mut state = valid_state();
    state.branches[0].rounds[0].calls[0].id = "i".repeat(crate::agent::MAX_TOOL_CALL_ID_BYTES + 1);
    assert!(corruption_message(&state).contains("tool call id exceeds its byte cap"));

    let mut state = valid_state();
    state.branches[0].rounds[0].calls[0].name = "n".repeat(crate::agent::MAX_TOOL_NAME_BYTES + 1);
    assert!(corruption_message(&state).contains("tool name exceeds its byte cap"));
}

#[test]
fn persisted_mapped_response_aggregate_cap_is_enforced() {
    let mut state = valid_state();
    let call_bytes = state.branches[0].rounds[0].calls[0].id.len()
        + state.branches[0].rounds[0].calls[0].name.len()
        + state.branches[0].rounds[0].calls[0].args.len();
    state.branches[0].rounds[0].assistant =
        "a".repeat(crate::agent::MAX_RESPONSE_BYTES - call_bytes + 1);
    assert!(
        corruption_message(&state).contains("mapped response exceeds the model response byte cap")
    );
}

#[test]
fn unknown_tools_and_impossible_completed_transcripts_are_rejected() {
    let mut state = valid_state();
    state.branches[0].rounds[0].calls[0].name = "unknown".into();
    assert!(corruption_message(&state).contains("unknown tool name"));

    let mut state = valid_state();
    state.branches[0].summary = "different".into();
    assert!(corruption_message(&state).contains("final no-tool round"));

    let mut state = valid_state();
    state.branches[0].rounds.pop();
    assert!(corruption_message(&state).contains("final no-tool round"));

    let mut state = valid_state();
    state.branches[0].rounds.insert(
        1,
        RoundRecord {
            assistant: "impossible early stop".into(),
            calls: Vec::new(),
        },
    );
    assert!(corruption_message(&state).contains("only a completed branch"));
}

#[test]
fn refused_calls_never_count_as_executed() {
    let mut state = valid_state();
    let template = state.branches[0].rounds[0].calls[0].clone();
    state.branches[0].rounds[0].calls = (0..crate::agent::MAX_TOOL_CALLS_PER_TURN)
        .map(|index| ToolCallRecord {
            id: format!("call-{index}"),
            ..template.clone()
        })
        .collect();
    state.branches[0].rounds[0].calls.push(ToolCallRecord {
        id: "call-refused".into(),
        refused: true,
        ..template.clone()
    });
    // Executed calls sit at the cap and the refusal rides on top: the store
    // no longer enforces a call-count constant because budgets are declared
    // per run, but refused records must never look like executed ones.
    state.validate().unwrap();

    let mut state = valid_state();
    state.branches[0].lifecycle = Lifecycle::Failed;
    state.branches[0].summary.clear();
    state.branches[0].error = "failed".into();
    state.branches[0].rounds = (0..=crate::agent::MAX_TURN_ROUNDS)
        .map(|index| RoundRecord {
            assistant: String::new(),
            calls: vec![ToolCallRecord {
                id: format!("round-{index}"),
                name: "read_file".into(),
                args: "{}".into(),
                ok: false,
                refused: false,
                result: String::new(),
            }],
        })
        .collect();
    assert!(corruption_message(&state).contains("too many assistant/tool rounds"));

    let mut state = valid_state();
    state.branches[0].rounds[0].calls[0].result = "r".repeat(crate::agent::MAX_TURN_OUTPUT_BYTES);
    state.validate().unwrap();
    state.branches[0].rounds[0].calls[0].result.push('r');
    assert!(corruption_message(&state).contains("tool results exceed"));

    let mut state = valid_state();
    let args_overhead = r#"{"x":""}"#.len();
    state.branches[0].rounds[0].calls[0].args = format!(
        "{{\"x\":\"{}\"}}",
        "a".repeat(crate::agent::MAX_TURN_ARGS_BYTES - args_overhead)
    );
    state.validate().unwrap();
    let insert_at = state.branches[0].rounds[0].calls[0].args.len() - 2;
    state.branches[0].rounds[0].calls[0]
        .args
        .insert(insert_at, 'a');
    assert!(corruption_message(&state).contains("tool arguments exceed"));
}

#[test]
fn prompt_summary_error_lease_and_lifecycle_fields_are_enforced() {
    let mut state = valid_state();
    state.branches[0].prompt = "p".repeat(MAX_PROMPT_BYTES + 1);
    state.branches[0].digest = crate::agent::prompt_digest(&state.branches[0].prompt);
    assert!(corruption_message(&state).contains("prompt exceeds"));

    let mut state = valid_state();
    state.branches[0].reserved_at = 2;
    state.branches[0].lease_expiry = 2;
    assert!(corruption_message(&state).contains("reservation lease"));

    // A terminal branch must not retain a live lease.
    let mut state = valid_state();
    state.branches[0].reserved_at = 1;
    state.branches[0].lease_expiry = 2;
    assert!(corruption_message(&state).contains("terminal branch retains"));

    // A pending branch must carry a live lease.
    let mut state = valid_state();
    state.branches[0].lifecycle = Lifecycle::Pending;
    state.branches[0].summary.clear();
    state.branches[0].rounds.clear();
    state.branches[0].owner = "owner".to_string();
    state.branches[0].reserved_at = 0;
    state.branches[0].lease_expiry = 0;
    assert!(corruption_message(&state).contains("invalid reservation lease"));

    let mut state = valid_state();
    state.branches[0].lifecycle = Lifecycle::Pending;
    state.branches[0].summary.clear();
    state.branches[0].rounds.clear();
    state.branches[0].owner = "o".repeat(MAX_RENDERED_FIELD_BYTES + 1);
    state.branches[0].reserved_at = 1;
    state.branches[0].lease_expiry = 2;
    assert!(corruption_message(&state).contains("owner token exceeds"));

    let mut state = valid_state();
    state.branches[0].lifecycle = Lifecycle::Failed;
    state.branches[0].summary.clear();
    state.branches[0].rounds.clear();
    state.branches[0].reserved_at = 0;
    state.branches[0].lease_expiry = 0;
    state.branches[0].error = "e".repeat(crate::redact::MAX_ERROR_TEXT_BYTES + 1);
    assert!(corruption_message(&state).contains("error exceeds"));
}
