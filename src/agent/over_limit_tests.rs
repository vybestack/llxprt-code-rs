//! Over-limit recovery verdict tests: compaction once, terminal failure twice.

use super::tests::shared_config_home;
use super::*;
use crate::adapter::{ChatBackend, LlmResult, LlmUsage};
use crate::session::{SessionId, SessionStore};
use crate::tools::ToolSpec;
use serdes_ai::core::FinishReason;
use std::sync::Mutex;

/// Scripted provider failures plus optional request capture for over-limit recovery.
struct RecoveryBackend {
    replies: Mutex<std::collections::VecDeque<Result<LlmResult, String>>>,
    requests: Mutex<Vec<Vec<serdes_ai::core::ModelRequest>>>,
}
impl RecoveryBackend {
    fn new(replies: Vec<Result<LlmResult, String>>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            requests: Mutex::new(Vec::new()),
        }
    }
}
impl ChatBackend for RecoveryBackend {
    fn request<'a>(
        &'a self,
        requests: &'a [serdes_ai::core::ModelRequest],
        _: &'a [ToolSpec],
    ) -> crate::adapter::ModelFuture<'a> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(requests.to_vec());
            self.replies.lock().unwrap().pop_front().unwrap()
        })
    }
    fn request_calls(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}
impl ChatBackend for std::sync::Arc<RecoveryBackend> {
    fn request<'a>(
        &'a self,
        requests: &'a [serdes_ai::core::ModelRequest],
        tools: &'a [ToolSpec],
    ) -> crate::adapter::ModelFuture<'a> {
        Box::pin(async move { (**self).request(requests, tools).await })
    }
    fn request_calls(&self) -> usize {
        (**self).request_calls()
    }
}
fn stop_reply() -> LlmResult {
    LlmResult {
        text: "done".into(),
        calls: Vec::new(),
        finish_reason: Some(FinishReason::Stop),
        usage: LlmUsage::default(),
    }
}
fn recovery_store_with_prompt(
    name: &str,
    prompt: &str,
) -> (tempfile::TempDir, SessionStore, ReservedRequest) {
    let cwd = tempfile::tempdir().unwrap();
    let _config = shared_config_home();
    let store = SessionStore::load(&SessionId::parse(name).unwrap()).unwrap();
    let reserved = store.start_request(None, None, prompt, cwd.path()).unwrap();
    (cwd, store, reserved)
}
fn recovery_store(name: &str) -> (tempfile::TempDir, SessionStore, ReservedRequest) {
    recovery_store_with_prompt(name, "current prompt verbatim")
}

#[test]
fn over_limit_preflight_compacts_once_and_proceeds() {
    let (cwd, store, mut reserved) = recovery_store("over-preflight");
    reserved.history.push(crate::session::HistoryTurn {
        turn: 0,
        attempt: 1,
        branch_id: "old".into(),
        prompt: "old".into(),
        rounds: vec![RoundRecord {
            assistant: "x".repeat(20_000),
            calls: Vec::new(),
        }],
        summary: String::new(),
    });
    let backend = std::sync::Arc::new(RecoveryBackend::new(vec![Ok(stop_reply())]));
    let agent =
        CodingAgent::with_backend(Box::new(backend.clone()), cwd.path().to_path_buf(), false)
            .with_context_limit(Some(10_000));
    assert_eq!(agent.run(&store, &reserved).unwrap().status, "ok");
    assert_eq!(agent.model_calls(), 1);
    let captured = backend.requests.lock().unwrap();
    assert!(serde_json::to_string(captured.last().unwrap())
        .unwrap()
        .contains("current prompt verbatim"));
}

#[test]
fn over_limit_provider_verdict_compacts_once_and_proceeds() {
    let (cwd, store, reserved) = recovery_store("over-provider");
    let agent = CodingAgent::with_backend(
        Box::new(RecoveryBackend::new(vec![
            Err("Model context length exceeded (1 tokens maximum, 2 requested)".into()),
            Ok(stop_reply()),
        ])),
        cwd.path().to_path_buf(),
        false,
    );
    assert_eq!(agent.run(&store, &reserved).unwrap().status, "ok");
    assert_eq!(agent.model_calls(), 2);
}

#[test]
fn over_limit_twice_fails_naming_both_attempts() {
    let prompt = "x".repeat(100_000);
    let (cwd, store, reserved) = recovery_store_with_prompt("over-twice", &prompt);
    let agent = CodingAgent::with_backend(
        Box::new(RecoveryBackend::new(vec![Ok(stop_reply())])),
        cwd.path().to_path_buf(),
        false,
    )
    .with_context_limit(Some(10_000));
    let error = agent.run(&store, &reserved).unwrap_err();
    assert_eq!(error.key, "context-limit");
    assert!(error.message.contains("estimated request would be "));
    assert!(error.message.contains("still "));
}

#[test]
fn provider_verdict_twice_fails_naming_both_attempts() {
    let (cwd, store, reserved) = recovery_store("provider-twice");
    let message = "Model context length exceeded (1 tokens maximum, 2 requested)".to_string();
    let agent = CodingAgent::with_backend(
        Box::new(RecoveryBackend::new(vec![
            Err(message.clone()),
            Err(message),
        ])),
        cwd.path().to_path_buf(),
        false,
    );
    let error = agent.run(&store, &reserved).unwrap_err();
    assert_eq!(error.key, "context-limit");
    assert_eq!(agent.model_calls(), 2);
    assert!(error.message.contains("attempt 1"));
    assert!(error.message.contains("retry after compaction"));
}

#[test]
fn unrelated_provider_error_untouched() {
    let (cwd, store, reserved) = recovery_store("provider-unrelated");
    let agent = CodingAgent::with_backend(
        Box::new(RecoveryBackend::new(vec![Err(
            "Model request rate limited (no retry delay supplied)".into(),
        )])),
        cwd.path().to_path_buf(),
        false,
    );
    let error = agent.run(&store, &reserved).unwrap_err();
    assert_eq!(agent.model_calls(), 1);
    assert_eq!(error.key, "model");
    assert!(!error.message.contains("context length exceeded"));
}
