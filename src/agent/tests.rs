use super::*;
use crate::adapter::{ChatBackend, ToolCall};
use crate::session::{Lifecycle, SessionId, SessionStore};
use crate::tools::ToolSpec;
use serdes_ai::core::FinishReason;
use std::path::PathBuf;
use std::sync::Mutex;

/// A scripted backend for the forced-summary accounting tests: each call pops the
/// next canned reply, repeating the last when exhausted.
struct MockBackend {
    replies: Mutex<std::collections::VecDeque<LlmResult>>,
    calls: Mutex<usize>,
}

impl MockBackend {
    fn new(replies: Vec<LlmResult>) -> Self {
        MockBackend {
            replies: Mutex::new(replies.into()),
            calls: Mutex::new(0),
        }
    }
}

impl ChatBackend for MockBackend {
    fn request(
        &self,
        _requests: &[serdes_ai::core::ModelRequest],
        _tools: &[ToolSpec],
    ) -> Result<LlmResult, String> {
        *self.calls.lock().unwrap() += 1;
        let mut q = self.replies.lock().unwrap();
        Ok(if let Some(r) = q.pop_front() {
            r
        } else {
            LlmResult {
                text: String::new(),
                calls: Vec::new(),
                finish_reason: Some(FinishReason::Stop),
            }
        })
    }

    fn request_calls(&self) -> usize {
        *self.calls.lock().unwrap()
    }
}

static SHARED_CONFIG_HOME: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

#[cfg(unix)]
extern "C" fn cleanup_shared_config_home() {
    if let Some(root) = SHARED_CONFIG_HOME.get().and_then(|path| path.parent()) {
        let _ = std::fs::remove_dir_all(root);
    }
}

/// A per-process config home shared by every unit test in this binary. Integration
/// binaries run in their own process, so no other binary mutates this root.
fn shared_config_home() -> PathBuf {
    SHARED_CONFIG_HOME
        .get_or_init(|| {
            let base =
                std::env::temp_dir().join(format!("llxprt-rs-agent-unit-{}", std::process::id()));
            let root = base.join("config");
            std::fs::create_dir_all(&root).unwrap();
            #[cfg(unix)]
            unsafe {
                libc::atexit(cleanup_shared_config_home);
            }
            unsafe {
                std::env::set_var("LLXPRT_CONFIG_HOME", &root);
            }
            root
        })
        .clone()
}

/// Build a test agent with the forced-summary relevant overrides: an explicit per-turn
/// round budget (the forced path's last round gate uses `max_rounds`). The forced
/// response size is injected via the canned backend reply, never a cap override.
fn agent_with_caps(
    backend: Box<dyn ChatBackend>,
    cwd: &std::path::Path,
    max_rounds: Option<usize>,
    context_limit: Option<u64>,
    _forced_assistant_bytes: Option<usize>,
) -> CodingAgent {
    let mut a = CodingAgent::with_backend(backend, cwd.to_path_buf(), false);
    if let Some(c) = context_limit {
        a = a.with_context_limit(Some(c));
    }
    if let Some(m) = max_rounds {
        a = a.with_max_rounds(m);
    }
    let _ = _forced_assistant_bytes;
    a
}

/// A forced response exactly at the remaining assistant cap (`exact` bytes after the
/// first round's `pre` bytes) succeeds and is persisted as the completed final round.
#[test]
fn forced_response_at_exact_remaining_assistant_cap_succeeds() {
    let cwd = tempfile::tempdir().unwrap();
    let cwd = cwd.path().join("ws-exact");
    std::fs::create_dir_all(&cwd).unwrap();
    let _cfg = shared_config_home();
    let st = SessionStore::load(&SessionId::parse("fcap-exact").unwrap()).unwrap();
    let r = st.start_request(None, None, "P", &cwd).unwrap();
    let pre = MAX_TURN_ASSISTANT_BYTES - 512;
    // A no-tool first round is a terminal success: the turn ends on its own text and
    // takes the `else` (finish) branch, with only 1 backend call. To drive the
    // forced path with a pre-loaded remaining cap we make the first round a tool round
    // (the text, not the tool, pre-fills the assistant aggregate) and the forced
    // summary the second call.
    let r1 = LlmResult {
        text: "x".repeat(pre),
        calls: vec![ToolCall {
            id: "c1".into(),
            name: "list_directory".into(),
            args_json: r#"{"path":"."}"#.into(),
        }],
        finish_reason: Some(FinishReason::ToolCall),
    };
    let r2 = LlmResult {
        text: "z".repeat(MAX_TURN_ASSISTANT_BYTES - pre),
        calls: Vec::new(),
        finish_reason: Some(FinishReason::Stop),
    };
    let a = agent_with_caps(
        Box::new(MockBackend::new(vec![r1, r2])),
        &cwd,
        Some(16),
        None,
        None,
    );
    let out = a.run(&st, &r).expect("exact remaining cap must succeed");
    assert_eq!(out.status, "ok");
    assert_eq!(
        a.model_calls(),
        2,
        "a forced round ran the second backend call"
    );
    let snap = st.snapshot().unwrap();
    let b = snap.branches[0].clone();
    assert_eq!(b.lifecycle, Lifecycle::Completed);
    assert_eq!(
        b.rounds.len(),
        2,
        "final summary persisted as the final round"
    );
    assert_eq!(b.summary.len(), MAX_TURN_ASSISTANT_BYTES - pre);
    assert_eq!(b.rounds[1].assistant.len(), MAX_TURN_ASSISTANT_BYTES - pre);
    assert!(!b.owner.is_empty(), "finalize retained the owner identity");
}

/// A forced response exactly at `MAX_TURN_ASSISTANT_BYTES` bytes after an empty
/// first round succeeds (a forced response filling the entire assistant cap is the
/// boundary), persisting a completed branch.
#[test]
fn forced_empty_first_round_succeeds_at_exact_remaining_cap() {
    let cwd = tempfile::tempdir().unwrap();
    let _r_cwd = cwd.path();
    std::fs::create_dir_all(_r_cwd).unwrap();
    let _cfg = shared_config_home();
    let st = SessionStore::load(&SessionId::parse("fcap-empty-pre").unwrap()).unwrap();
    {
        let r = st.start_request(None, None, "P", _r_cwd).unwrap();
        let r = &r;
        let r2 = LlmResult {
            text: "é".repeat(MAX_TURN_ASSISTANT_BYTES / "é".len()),
            calls: Vec::new(),
            finish_reason: Some(FinishReason::Stop),
        };
        let a = CodingAgent::with_backend(
            Box::new(MockBackend::new(vec![
                LlmResult {
                    text: String::new(),
                    calls: Vec::new(),
                    finish_reason: Some(FinishReason::Stop),
                },
                r2,
            ])),
            cwd.path().to_path_buf(),
            false,
        );
        let out = a
            .run(&st, r)
            .expect("a forced response at the full assistant cap must succeed");
        let _ = r;
        assert_eq!(out.status, "ok");
        assert_eq!(a.model_calls(), 2);
        let snap = st.snapshot().unwrap();
        assert_eq!(snap.branches[0].summary.len(), MAX_TURN_ASSISTANT_BYTES);
    }
}

/// A forced response one byte over the remaining assistant cap exits terminal `turn-budget`,
/// the branch is marked Failed with no persisted round (no session-persist), and the
/// owner is cleared (no live owner).
#[test]
fn forced_response_at_remaining_cap_plus_one_fails_terminally() {
    let cwd = tempfile::tempdir().unwrap();
    let cwd = cwd.path().join("ws-cap-1");
    std::fs::create_dir_all(&cwd).unwrap();
    let _cfg = shared_config_home();
    let st = SessionStore::load(&SessionId::parse("fcap-p1").unwrap()).unwrap();
    let pre = MAX_TURN_ASSISTANT_BYTES - 512;
    let rr1 = LlmResult {
        text: "x".repeat(pre),
        calls: vec![ToolCall {
            id: "c9".into(),
            name: "list_directory".into(),
            args_json: r#"{"path":"."}"#.into(),
        }],
        finish_reason: Some(FinishReason::ToolCall),
    };
    let rr2 = LlmResult {
        text: "z".repeat(MAX_TURN_ASSISTANT_BYTES - pre + 1),
        calls: Vec::new(),
        finish_reason: Some(FinishReason::Stop),
    };
    let ra = CodingAgent::with_backend(
        Box::new(MockBackend::new(vec![rr1, rr2])),
        cwd.clone(),
        false,
    );
    let rr = st.start_request(None, None, "P", &cwd).unwrap();
    let e = ra.run(&st, &rr).expect_err("cap + 1 must terminally fail");
    let _ = rr;
    assert_eq!(e.key, "turn-budget");
    assert!(e.message.contains("assistant content"), "{}", e.message);
    assert_eq!(e.code, crate::cli::Code::Model);
    let snap = st.snapshot().unwrap();
    let b = snap.branches[0].clone();
    assert_eq!(b.lifecycle, Lifecycle::Failed);
    assert!(b.error.contains("assistant content"), "{}", b.error);
    assert!(!b.owner.is_empty(), "failure retained the owner identity");
}

#[test]
fn system_prompt_formats_reasoning_context_once() {
    let prompt = coding_system_prompt(
        std::path::Path::new("/workspace"),
        "use care",
        false,
        Some(16),
    );
    assert_eq!(prompt.matches("Reasoning context:").count(), 1);
    assert!(prompt.contains("Reasoning context: use care\n"));
}

#[test]
fn system_prompt_states_the_resolved_tool_budget() {
    let bounded = coding_system_prompt(std::path::Path::new("/w"), "", false, Some(48));
    assert!(bounded.contains("at most 48 tool calls"));
    let unbounded = coding_system_prompt(std::path::Path::new("/w"), "", false, None);
    assert!(unbounded.contains("no fixed tool-call limit"));
}

#[test]
fn budget_notice_reports_remaining_and_escalates_near_the_end() {
    use crate::agent::budget_notice;
    assert_eq!(
        budget_notice(Some(16), 4),
        "[budget: 12 of 16 tool calls left]"
    );
    assert!(budget_notice(Some(16), 13)
        .contains("only 3 of 16 tool calls left; wrap up and produce your final summary"));
    assert!(budget_notice(Some(16), 16).contains("reply with your final summary only"));
    assert_eq!(budget_notice(None, 999), "");
}

#[test]
fn reflected_secret_in_assistant_text_fails_without_persisting_it() {
    let cwd = tempfile::tempdir().unwrap();
    let _config = shared_config_home();
    let store = SessionStore::load(&SessionId::parse("secret-text").unwrap()).unwrap();
    let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    let secret = "accepted-secret-value";
    let reply = LlmResult {
        text: format!("reflected {secret}"),
        calls: Vec::new(),
        finish_reason: Some(FinishReason::Stop),
    };
    let agent = CodingAgent::with_backend(
        Box::new(MockBackend::new(vec![reply])),
        cwd.path().to_path_buf(),
        false,
    )
    .with_secrets(vec![secret.to_string()]);
    let error = agent
        .run(&store, &reserved)
        .expect_err("secret reflection must fail");
    assert_eq!(
        error.message,
        "model response contained a configured secret"
    );
    let state = store.snapshot().unwrap();
    assert!(!serde_json::to_string(&state).unwrap().contains(secret));
}

#[test]
fn reflected_secret_in_tool_args_prevents_tool_side_effect() {
    let cwd = tempfile::tempdir().unwrap();
    let _config = shared_config_home();
    let store = SessionStore::load(&SessionId::parse("secret-args").unwrap()).unwrap();
    let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    let secret = "accepted-secret-argument";
    let reply = LlmResult {
        text: String::new(),
        calls: vec![ToolCall {
            id: "c1".into(),
            name: "write_file".into(),
            args_json: format!(r#"{{"path":"leak.txt","content":"{secret}"}}"#),
        }],
        finish_reason: Some(FinishReason::ToolCall),
    };
    let agent = CodingAgent::with_backend(
        Box::new(MockBackend::new(vec![reply])),
        cwd.path().to_path_buf(),
        false,
    )
    .with_secrets(vec![secret.to_string()]);
    let error = agent
        .run(&store, &reserved)
        .expect_err("secret reflection must fail");
    assert_eq!(
        error.message,
        "model response contained a configured secret"
    );
    assert!(!cwd.path().join("leak.txt").exists());
}

#[test]
fn oversized_tool_call_id_is_rejected_before_side_effect() {
    let cwd = tempfile::tempdir().unwrap();
    let _config = shared_config_home();
    let store = SessionStore::load(&SessionId::parse("huge-call-id").unwrap()).unwrap();
    let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    let reply = LlmResult {
        text: String::new(),
        calls: vec![ToolCall {
            id: "i".repeat(MAX_TOOL_CALL_ID_BYTES + 1),
            name: "write_file".into(),
            args_json: r#"{"path":"created.txt","content":"bad"}"#.into(),
        }],
        finish_reason: Some(FinishReason::ToolCall),
    };
    let agent = CodingAgent::with_backend(
        Box::new(MockBackend::new(vec![reply])),
        cwd.path().to_path_buf(),
        false,
    );
    let error = agent
        .run(&store, &reserved)
        .expect_err("oversized id must fail");
    assert!(error.message.contains("tool call id exceeds"));
    assert!(!cwd.path().join("created.txt").exists());
}

#[test]
fn tool_output_is_scrubbed_before_model_return_and_persistence() {
    let cwd = tempfile::tempdir().unwrap();
    let secret = "workspace-secret-value";
    std::fs::write(cwd.path().join("secret.txt"), secret).unwrap();
    let _config = shared_config_home();
    let store = SessionStore::load(&SessionId::parse("secret-tool-output").unwrap()).unwrap();
    let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    let tool_round = LlmResult {
        text: String::new(),
        calls: vec![ToolCall {
            id: "c1".into(),
            name: "read_file".into(),
            args_json: r#"{"path":"secret.txt"}"#.into(),
        }],
        finish_reason: Some(FinishReason::ToolCall),
    };
    let final_round = LlmResult {
        text: "done".into(),
        calls: Vec::new(),
        finish_reason: Some(FinishReason::Stop),
    };
    let agent = CodingAgent::with_backend(
        Box::new(MockBackend::new(vec![tool_round, final_round])),
        cwd.path().to_path_buf(),
        false,
    )
    .with_secrets(vec![secret.to_string()]);
    agent.run(&store, &reserved).unwrap();
    let state = store.snapshot().unwrap();
    assert!(!serde_json::to_string(&state).unwrap().contains(secret));
    assert!(state.branches[0].rounds[0].calls[0]
        .result
        .contains("[redacted]"));
}

#[test]
fn normal_summary_after_maximum_tool_round_exceeds_cap() {
    let cwd = tempfile::tempdir().unwrap();
    let _config = shared_config_home();
    let store = SessionStore::load(&SessionId::parse("normal-round-limit").unwrap()).unwrap();
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
    let final_round = LlmResult {
        text: "summary".into(),
        calls: Vec::new(),
        finish_reason: Some(FinishReason::Stop),
    };
    let agent = CodingAgent::with_backend(
        Box::new(MockBackend::new(vec![tool_round, final_round])),
        cwd.path().to_path_buf(),
        false,
    )
    .with_max_rounds(1);
    let error = agent
        .run(&store, &reserved)
        .expect_err("normal final response must exceed cap");
    assert_eq!(error.key, "turn-budget");
    assert_eq!(
        error.message,
        "turn would exceed the 1 round cap; give a final summary instead"
    );
    assert_eq!(agent.model_calls(), 2);
}

#[test]
fn forced_summary_counts_already_persisted_rounds() {
    let cwd = tempfile::tempdir().unwrap();
    let _config = shared_config_home();
    let store = SessionStore::load(&SessionId::parse("forced-round-limit").unwrap()).unwrap();
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
    let empty_round = LlmResult {
        text: String::new(),
        calls: Vec::new(),
        finish_reason: Some(FinishReason::Stop),
    };
    let forced_round = LlmResult {
        text: "summary".into(),
        calls: Vec::new(),
        finish_reason: Some(FinishReason::Stop),
    };
    let agent = CodingAgent::with_backend(
        Box::new(MockBackend::new(vec![
            tool_round,
            empty_round,
            forced_round,
        ])),
        cwd.path().to_path_buf(),
        false,
    )
    .with_max_rounds(1);
    let error = agent
        .run(&store, &reserved)
        .expect_err("forced round must exceed cap");
    assert_eq!(error.key, "turn-budget");
    assert_eq!(agent.model_calls(), 3);
}
