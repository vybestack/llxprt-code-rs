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

fn materialized_request_fixture(
    prompt: &str,
) -> (Vec<serdes_ai::core::ModelRequest>, Vec<ToolSpec>) {
    let agent = CodingAgent::with_backend(
        Box::new(MockBackend::new(Vec::new())),
        std::env::temp_dir(),
        false,
    );
    let reserved = ReservedRequest {
        branch_id: "b1".into(),
        turn: 1,
        attempt: 1,
        replay: false,
        retry: false,
        rounds: Vec::new(),
        summary: String::new(),
        prompt: prompt.into(),
        history: Vec::new(),
        owner: "owner".into(),
    };
    (
        agent.materialize_requests(&reserved),
        crate::tools::tool_specs(false),
    )
}

/// The complete-request preflight includes the fixed overhead (model id, route,
/// provider settings, framing) so a request of only part bytes can still trip the
/// conservative budget gate; it is refused before any backend call.
#[test]
fn estimate_request_bytes_includes_fixed_overhead() {
    let (reqs, tools) = materialized_request_fixture("a");
    // This is the exact initial request emitted by CodingAgent: its system prompt and
    // user prompt, tool schemas, fixed overhead, request framing, and part framing.
    let est = estimate_request_bytes(&reqs);
    assert!(est > 256);
    assert!(
        round_budget_exceeded(&reqs, &tools, Some(10)),
        "a tiny context budget must refuse the complete request"
    );
    assert!(
        !round_budget_exceeded(&reqs, &tools, None),
        "the default 32MiB budget still accepts a one-prompt request"
    );
    assert_eq!(REQUEST_FIXED_OVERHEAD_BYTES, 8192);
    assert_eq!(
        REQUEST_FIXED_OVERHEAD_BYTES,
        crate::redact::MAX_ERROR_TEXT_BYTES
    );
    assert_eq!(crate::redact::TRUNCATION_MARKER, "[truncated]");
}

#[test]
fn exact_materialized_request_budget_is_inclusive() {
    let mut prompt = String::from("boundary");
    let (requests, tools, total) = loop {
        let (requests, tools) = materialized_request_fixture(&prompt);
        let total = estimate_request_bytes(&requests)
            .saturating_add(crate::adapter::estimate_tool_schema_bytes(&tools));
        if total.checked_rem(3) == Some(0) {
            break (requests, tools, total);
        }
        prompt.push('x');
    };
    let exact_tokens = u64::try_from(total / 3).unwrap();
    assert!(!round_budget_exceeded(
        &requests,
        &tools,
        Some(exact_tokens)
    ));
    assert!(round_budget_exceeded(
        &requests,
        &tools,
        Some(exact_tokens - 1)
    ));
}

#[test]
fn omitting_the_system_prompt_changes_a_budget_decision() {
    let (requests, tools) = materialized_request_fixture("a");
    let incomplete = vec![user_request("a")];
    let incomplete_total = estimate_request_bytes(&incomplete)
        .saturating_add(crate::adapter::estimate_tool_schema_bytes(&tools));
    let fixture_tokens = u64::try_from(incomplete_total.div_ceil(3)).unwrap();
    assert!(!round_budget_exceeded(
        &incomplete,
        &tools,
        Some(fixture_tokens)
    ));
    assert!(round_budget_exceeded(
        &requests,
        &tools,
        Some(fixture_tokens)
    ));
}
/// A giant prompt request stays over the complete-request budget: the saturated
/// estimate cannot wrap to a smaller total, so an over-cap complete request is refused
/// before any backend call. (A single small request stays *under* the default 32 MiB
/// budget regardless of the 8192-byte fixed overhead.)
#[test]
fn estimate_request_bytes_saturates_and_refuses_oversized() {
    let (reqs, tools) = materialized_request_fixture(&"x".repeat(16 * 1024 * 1024));
    assert!(estimate_request_bytes(&reqs) > 16 * 1024 * 1024);
    assert!(
        round_budget_exceeded(&reqs, &tools, Some(7)),
        "the configured 7-token context-limit (a 21-byte budget) has a tiny byte budget and so refuses a 16 MiB request"
    );
    // A single small request stays under the default 32 MiB budget.
    let (tiny, tiny_tools) = materialized_request_fixture("a");
    let _ = estimate_request_bytes(&tiny);
    assert!(
        !round_budget_exceeded(&tiny, &tiny_tools, None),
        "a single small request stays under the default budget"
    );
}

#[test]
fn oversized_history_is_outside_the_budget() {
    let huge = crate::session::HistoryTurn {
        turn: 1,
        attempt: 1,
        branch_id: "b1".into(),
        prompt: "x".repeat(1_000),
        rounds: vec![],
        summary: String::new(),
    };
    assert!(history_needs_check(std::slice::from_ref(&huge)));
    assert!(!history_within(std::slice::from_ref(&huge), 10));
    assert!(history_within(&[huge], 10 * 1024 * 1024));
    assert!(!history_needs_check(&[]));
}

#[test]
fn model_response_cap_is_a_positive_bound() {
    assert_ne!(MAX_RESPONSE_BYTES, 0);
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
    assert!(b.owner.is_empty(), "finalize cleared the owner token");
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
    assert!(b.owner.is_empty(), "the failed branch released its owner");
}

#[test]
fn system_prompt_formats_reasoning_context_once() {
    let prompt = coding_system_prompt(std::path::Path::new("/workspace"), "use care", false);
    assert_eq!(prompt.matches("Reasoning context:").count(), 1);
    assert!(prompt.contains("Reasoning context: use care\n"));
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
