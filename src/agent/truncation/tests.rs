//! Turn-retry tests for a first-completion output truncation (issue 153).

use super::*;
use crate::agent::tests::{shared_config_home, MockBackend};
use crate::session::{Lifecycle, SessionId, SessionStore};

/// A truncated completion: cut by the output cap, no tool call parsed.
fn truncated(text: &str) -> LlmResult {
    LlmResult {
        text: text.into(),
        calls: Vec::new(),
        finish_reason: Some(FinishReason::Length),
    }
}

/// A healthy reply that follows one truncation.
fn healthy(text: &str) -> LlmResult {
    LlmResult {
        text: text.into(),
        calls: Vec::new(),
        finish_reason: Some(FinishReason::Stop),
    }
}

/// A first-turn truncation followed by a healthy completion completes the run normally:
/// exactly one re-issue, no failed lifecycle, and the truncated bytes neither persisted
/// as a round nor counted as assistant output.
#[test]
fn first_turn_truncation_then_healthy_completion_succeeds() {
    let cwd = tempfile::tempdir().unwrap();
    let _config = shared_config_home();
    let store = SessionStore::load(&SessionId::parse("trunc-ok-1").unwrap()).unwrap();
    let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    let agent = CodingAgent::with_backend(
        Box::new(MockBackend::new(vec![
            truncated("a very long plan that ran out of tokens"),
            healthy("done"),
        ])),
        cwd.path().to_path_buf(),
        false,
    );
    let run = agent
        .run(&store, &reserved)
        .expect("retried turn completes");
    assert_eq!(run.status, "ok");
    assert_eq!(agent.model_calls(), 2, "exactly one re-issue");
    assert_eq!(run.summary, "done");
    assert_eq!(run.zero_call_tail, 1);
    assert_eq!(run.terminal_outcome, None);
    let snapshot = store.snapshot().unwrap();
    assert_eq!(snapshot.branches[0].lifecycle, Lifecycle::Completed);
}

/// The re-issue survives a truncated completion that carries tool calls: the calls are
/// never executed, so the retry is still safe and the healthy reply finishes the turn.
#[test]
fn truncated_first_turn_with_calls_is_not_executed_before_retry() {
    let cwd = tempfile::tempdir().unwrap();
    let _config = shared_config_home();
    let store = SessionStore::load(&SessionId::parse("trunc-calls-1").unwrap()).unwrap();
    let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    let truncated_with_call = LlmResult {
        text: "plan".into(),
        calls: vec![ToolCall {
            id: "c1".into(),
            name: "list_directory".into(),
            args_json: r#"{"path":"."}"#.into(),
        }],
        finish_reason: Some(FinishReason::Length),
    };
    let agent = CodingAgent::with_backend(
        Box::new(MockBackend::new(vec![truncated_with_call, healthy("done")])),
        cwd.path().to_path_buf(),
        false,
    );
    let run = agent
        .run(&store, &reserved)
        .expect("retried turn completes");
    assert_eq!(run.status, "ok");
    assert_eq!(run.tool_count, 0, "a truncated call never executes");
    assert_eq!(agent.model_calls(), 2);
}

/// Two consecutive truncations fail with the existing finish-reason error, its
/// remediation text intact, and the distinct exhausted-retry terminal outcome. Exactly
/// two model calls run, never a loop.
#[test]
fn two_consecutive_truncations_fail_with_the_distinct_outcome() {
    let cwd = tempfile::tempdir().unwrap();
    let _config = shared_config_home();
    let store = SessionStore::load(&SessionId::parse("trunc-fail-1").unwrap()).unwrap();
    let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    let agent = CodingAgent::with_backend(
        Box::new(MockBackend::new(vec![
            truncated("first"),
            truncated("second"),
        ])),
        cwd.path().to_path_buf(),
        false,
    );
    let error = agent
        .run(&store, &reserved)
        .expect_err("an exhausted retry stays terminal");
    assert_eq!(error.code, crate::envelope::Code::Model);
    assert_eq!(error.key, "finish-reason");
    assert!(error.message.contains("maxOutputTokens"), "{error}");
    assert_eq!(error.terminal_outcome, Some(TRUNCATED_OUTPUT_RETRIED_KEY));
    assert_eq!(agent.model_calls(), 2, "one retry, then stop");
    let snapshot = store.snapshot().unwrap();
    assert_eq!(snapshot.branches[0].lifecycle, Lifecycle::Failed);
}

/// A truncation on a later round, after tool calls have run, keeps the existing fatal
/// path: no re-issue, no terminal outcome, no attempt to replay lost tool context.
#[test]
fn mid_work_truncation_after_tool_calls_stays_fatal() {
    let cwd = tempfile::tempdir().unwrap();
    let _config = shared_config_home();
    let store = SessionStore::load(&SessionId::parse("trunc-mid-1").unwrap()).unwrap();
    let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    let tool_round = LlmResult {
        text: String::new(),
        calls: vec![ToolCall {
            id: "c1".into(),
            name: "list_directory".into(),
            args_json: r#"{"path":"."}"#.into(),
        }],
        finish_reason: Some(FinishReason::ToolCall),
    };
    let agent = CodingAgent::with_backend(
        Box::new(MockBackend::new(vec![tool_round, truncated("half")])),
        cwd.path().to_path_buf(),
        false,
    );
    let error = agent
        .run(&store, &reserved)
        .expect_err("mid-work truncation loses tool context");
    assert_eq!(error.key, "finish-reason");
    assert_eq!(error.terminal_outcome, None);
    assert_eq!(agent.model_calls(), 2, "the second call is the next round");
    let snapshot = store.snapshot().unwrap();
    assert_eq!(snapshot.branches[0].lifecycle, Lifecycle::Failed);
}

/// Only a truncation on a turn with no persisted tool round is retryable; a truncation
/// after tool work, and any non-truncated reply, are not.
#[test]
fn only_a_pre_tool_truncation_is_retryable() {
    let round = crate::session::RoundRecord {
        assistant: "r".into(),
        calls: Vec::new(),
    };
    assert!(retryable(&[], &truncated("half")));
    assert!(!retryable(&[round], &truncated("half")));
    assert!(!retryable(&[], &healthy("done")));
}

/// A retry that comes back with tool calls continues the turn normally: the nudge stays in
/// the request list and the re-issued reply drives ordinary tool work.
#[test]
fn retried_turn_can_continue_into_tool_work() {
    let cwd = tempfile::tempdir().unwrap();
    let _config = shared_config_home();
    let store = SessionStore::load(&SessionId::parse("trunc-tool-1").unwrap()).unwrap();
    let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    let tool_round = LlmResult {
        text: String::new(),
        calls: vec![ToolCall {
            id: "c1".into(),
            name: "list_directory".into(),
            args_json: r#"{"path":"."}"#.into(),
        }],
        finish_reason: Some(FinishReason::ToolCall),
    };
    let agent = CodingAgent::with_backend(
        Box::new(MockBackend::new(vec![
            truncated("a long plan that ran out of tokens"),
            tool_round,
            healthy("done"),
        ])),
        cwd.path().to_path_buf(),
        false,
    );
    let run = agent
        .run(&store, &reserved)
        .expect("retried turn completes");
    assert_eq!(run.status, "ok");
    assert_eq!(run.tool_count, 1);
    assert_eq!(agent.model_calls(), 3);
    let snapshot = store.snapshot().unwrap();
    assert_eq!(snapshot.branches[0].lifecycle, Lifecycle::Completed);
    assert_eq!(
        snapshot.branches[0].rounds.len(),
        2,
        "the truncated completion is not persisted as a round"
    );
    assert!(
        !snapshot.branches[0]
            .rounds
            .iter()
            .any(|round| round.assistant.contains("ran out of tokens")),
        "no truncated text reaches the transcript"
    );
}
