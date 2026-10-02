//! Focused tests for agent tool-call validation and correctable refusals.

use super::tests::{shared_config_home, MockBackend};
use super::*;
use crate::adapter::{LlmUsage, ToolCall};
use crate::session::{SessionId, SessionStore};
use serdes_ai::core::FinishReason;

#[test]
fn unknown_tool_name_gets_corrective_result_and_turn_continues() {
    let cwd = tempfile::tempdir().unwrap();
    let _config = shared_config_home();
    let store = SessionStore::load(&SessionId::parse("unknown-tool-recovery").unwrap()).unwrap();
    let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    let unknown = LlmResult {
        text: String::new(),
        calls: vec![ToolCall {
            id: "unknown-1".into(),
            name: "search_file_command".into(),
            args_json: r#"{"path":".","pattern":"x"}"#.into(),
        }],
        finish_reason: Some(FinishReason::ToolCall),

        usage: LlmUsage::default(),
    };
    let read = LlmResult {
        text: String::new(),
        calls: vec![ToolCall {
            id: "read-1".into(),
            name: "read_file".into(),
            args_json: r#"{"path":"missing"}"#.into(),
        }],
        finish_reason: Some(FinishReason::ToolCall),

        usage: LlmUsage::default(),
    };
    let done = LlmResult {
        text: "done".into(),
        calls: Vec::new(),
        finish_reason: Some(FinishReason::Stop),

        usage: LlmUsage::default(),
    };
    let agent = CodingAgent::with_backend(
        Box::new(MockBackend::new(vec![unknown, read, done])),
        cwd.path().to_path_buf(),
        false,
    );
    assert_eq!(agent.run(&store, &reserved).unwrap().status, "ok");
    assert_eq!(agent.model_calls(), 3);
    let call = &store.snapshot().unwrap().branches[0].rounds[0].calls[0];
    assert_eq!((call.ok, call.refused), (false, false));
    assert!(call.result.contains("search_file_command"));
    assert!(call.result.contains("search_file_content"));
}

#[test]
fn unknown_tool_refusal_counts_toward_the_tool_budget() {
    let cwd = tempfile::tempdir().unwrap();
    let _config = shared_config_home();
    let store = SessionStore::load(&SessionId::parse("unknown-tool-budget").unwrap()).unwrap();
    let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    let unknown = LlmResult {
        text: String::new(),
        calls: vec![ToolCall {
            id: "unknown-1".into(),
            name: "search_file_command".into(),
            args_json: r#"{"path":".","pattern":"x"}"#.into(),
        }],
        finish_reason: Some(FinishReason::ToolCall),
        usage: LlmUsage::default(),
    };
    let read = LlmResult {
        text: String::new(),
        calls: vec![ToolCall {
            id: "read-1".into(),
            name: "read_file".into(),
            args_json: r#"{"path":"missing"}"#.into(),
        }],
        finish_reason: Some(FinishReason::ToolCall),
        usage: LlmUsage::default(),
    };
    let done = LlmResult {
        text: "done".into(),
        calls: Vec::new(),
        finish_reason: Some(FinishReason::Stop),
        usage: LlmUsage::default(),
    };
    let agent = CodingAgent::with_backend(
        Box::new(MockBackend::new(vec![unknown, read, done])),
        cwd.path().to_path_buf(),
        false,
    )
    .with_max_tool_calls(Some(2));
    let run = agent.run(&store, &reserved).unwrap();
    assert_eq!(run.status, "ok");
    // The hallucinated name and the valid call both consume budget.
    assert_eq!(run.tool_count, 2);
    let round = &store.snapshot().unwrap().branches[0].rounds[0];
    assert_eq!(round.calls.len(), 1);
    assert_eq!((round.calls[0].ok, round.calls[0].refused), (false, false));
}

#[test]
fn disabled_shell_tool_gets_corrective_result() {
    let cwd = tempfile::tempdir().unwrap();
    let _config = shared_config_home();
    let store = SessionStore::load(&SessionId::parse("disabled-shell-recovery").unwrap()).unwrap();
    let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    let shell = LlmResult {
        text: String::new(),
        calls: vec![ToolCall {
            id: "shell-1".into(),
            name: "run_shell_command".into(),
            args_json: r#"{"command":"true","timeout_seconds":1}"#.into(),
        }],
        finish_reason: Some(FinishReason::ToolCall),

        usage: LlmUsage::default(),
    };
    let done = LlmResult {
        text: "done".into(),
        calls: Vec::new(),
        finish_reason: Some(FinishReason::Stop),

        usage: LlmUsage::default(),
    };
    let agent = CodingAgent::with_backend(
        Box::new(MockBackend::new(vec![shell, done])),
        cwd.path().to_path_buf(),
        false,
    );
    assert_eq!(agent.run(&store, &reserved).unwrap().status, "ok");
    let call = &store.snapshot().unwrap().branches[0].rounds[0].calls[0];
    assert_eq!((call.ok, call.refused), (false, false));
    assert!(!call
        .result
        .split("available: ")
        .nth(1)
        .unwrap()
        .contains("run_shell_command"));
}

fn assert_invalid_tool_call(call: ToolCall) {
    let cwd = tempfile::tempdir().unwrap();
    let _config = shared_config_home();
    let store = SessionStore::load(
        &SessionId::parse(&format!("invalid-tool-{}", call.id.replace(' ', "blank"))).unwrap(),
    )
    .unwrap();
    let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    let reply = LlmResult {
        text: String::new(),
        calls: vec![call],
        finish_reason: Some(FinishReason::ToolCall),

        usage: LlmUsage::default(),
    };
    let agent = CodingAgent::with_backend(
        Box::new(MockBackend::new(vec![reply])),
        cwd.path().to_path_buf(),
        false,
    );
    assert_eq!(
        agent.run(&store, &reserved).unwrap_err().key,
        "invalid-tool-call"
    );
}

#[test]
fn empty_tool_call_id_still_fatal() {
    assert_invalid_tool_call(ToolCall {
        id: " ".into(),
        name: "read_file".into(),
        args_json: "{}".into(),
    });
}

#[test]
fn duplicate_tool_call_id_still_fatal() {
    let cwd = tempfile::tempdir().unwrap();
    let _config = shared_config_home();
    let store = SessionStore::load(&SessionId::parse("duplicate-tool-call").unwrap()).unwrap();
    let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    let call = ToolCall {
        id: "duplicate".into(),
        name: "read_file".into(),
        args_json: "{}".into(),
    };
    let reply = LlmResult {
        text: String::new(),
        calls: vec![call.clone(), call],
        finish_reason: Some(FinishReason::ToolCall),

        usage: LlmUsage::default(),
    };
    let agent = CodingAgent::with_backend(
        Box::new(MockBackend::new(vec![reply])),
        cwd.path().to_path_buf(),
        false,
    );
    assert_eq!(
        agent.run(&store, &reserved).unwrap_err().key,
        "invalid-tool-call"
    );
}

#[test]
fn malformed_tool_args_still_fatal() {
    assert_invalid_tool_call(ToolCall {
        id: "malformed".into(),
        name: "read_file".into(),
        args_json: "not json".into(),
    });
}
