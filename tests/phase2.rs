//! Phase 2 tests: versioned session storage, branch/turn/replay/lease/concurrency
//! semantics, full independent-process history continuation, and agent protocol
//! validation, all driven through a mock [`ChatBackend`] — no network.
//!
//! `cargo test` runs this binary's tests in one process with many threads, so the
//! daemon config dir is set **once** to a shared per-process root and every test uses a
//! unique session id under it. No test mutates the env afterwards.

use llxprt_code_rs::adapter::{ChatBackend, LlmResult, ToolCall};
use llxprt_code_rs::agent::CodingAgent;
use llxprt_code_rs::session::{
    BranchRecord, HistoryTurn, Lifecycle, ReservedRequest, RoundRecord, SessionId, SessionState,
    SessionStore, StoreError, ToolCallRecord,
};
use llxprt_code_rs::tools::ToolSpec;
use serdes_ai::core::FinishReason;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// A scripted backend: each call pops the next canned reply, repeating the last when
/// exhausted. Records every request so tests can assert what a later turn materialized.
struct MockBackend {
    replies: Mutex<std::collections::VecDeque<LlmResult>>,
    calls: Mutex<usize>,
    observer: Option<Box<dyn Fn(usize)>>,
}

fn result(text: &str) -> LlmResult {
    LlmResult {
        text: text.to_string(),
        calls: Vec::new(),
        finish_reason: Some(FinishReason::Stop),
    }
}

impl MockBackend {
    fn new(replies: Vec<LlmResult>) -> Self {
        MockBackend {
            replies: Mutex::new(replies.into()),
            calls: Mutex::new(0),
            observer: None,
        }
    }

    fn with_observer(mut self, observer: impl Fn(usize) + 'static) -> Self {
        self.observer = Some(Box::new(observer));
        self
    }
}

impl ChatBackend for MockBackend {
    fn request(
        &self,
        _requests: &[serdes_ai::core::ModelRequest],
        _tools: &[ToolSpec],
    ) -> Result<LlmResult, String> {
        let call_number = {
            let mut calls = self.calls.lock().unwrap();
            *calls += 1;
            *calls
        };
        if let Some(observer) = &self.observer {
            observer(call_number);
        }
        let mut q = self.replies.lock().unwrap();
        Ok(if let Some(r) = q.pop_front() {
            r
        } else {
            result("fallback")
        })
    }

    fn request_calls(&self) -> usize {
        *self.calls.lock().unwrap()
    }
}

static SHARED_ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

#[cfg(unix)]
extern "C" fn cleanup_shared_root() {
    if let Some(root) = SHARED_ROOT.get().and_then(|path| path.parent()) {
        let _ = std::fs::remove_dir_all(root);
    }
}

/// Shared per-process sessions root, removed when this test binary exits.
fn shared_root() -> PathBuf {
    SHARED_ROOT
        .get_or_init(|| {
            let base =
                std::env::temp_dir().join(format!("llxprt-rs-phase2-{}", std::process::id()));
            let root = base.join("config");
            std::fs::create_dir_all(&root).unwrap();
            #[cfg(unix)]
            unsafe {
                libc::atexit(cleanup_shared_root);
            }
            unsafe {
                std::env::set_var("LLXPRT_CONFIG_HOME", &root);
            }
            root
        })
        .clone()
}

fn store(id: &str) -> SessionStore {
    let _ = shared_root();
    let sid = SessionId::parse(id).unwrap();
    SessionStore::load(&sid).expect("open store")
}

fn new_cwd() -> PathBuf {
    let r = shared_root();
    let w = r.join(format!(
        "ws-{}",
        std::process::id().to_string() + exec_counter()
    ));
    std::fs::create_dir_all(&w).unwrap();
    w
}

fn workspace_identity(path: &Path) -> (u64, u64) {
    llxprt_code_rs::tools::WorkspaceCap::open(path)
        .unwrap()
        .identity()
}

fn exec_counter() -> &'static str {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    Box::leak(format!("-{n}").into_boxed_str())
}

fn reserved(
    store: &SessionStore,
    turn: Option<u32>,
    branch: Option<&str>,
    prompt: &str,
    cwd: &Path,
) -> Result<ReservedRequest, StoreError> {
    store.start_request(turn, branch, prompt, cwd)
}

fn agent(backend: Box<dyn ChatBackend>, cwd: &Path) -> CodingAgent {
    CodingAgent::with_backend(backend, cwd.to_path_buf(), false)
}

/// The `content-…` handle carried by a compact `CTXDIGEST` record.
fn digest_handle(record: &str) -> &str {
    let header = record
        .lines()
        .next()
        .expect("digest record carries a header line");
    let at = header
        .find("handle=")
        .expect("digest record carries a content handle")
        + "handle=".len();
    &header[at..]
}

/// Reads one published `context/` artifact of a session.
fn context_artifact(st: &SessionStore, name: &str) -> Vec<u8> {
    std::fs::read(st.session_dir.join("context").join(name))
        .unwrap_or_else(|error| panic!("read context artifact {name} failed: {error}"))
}

#[test]
fn no_tool_turn_persists_final_response_and_turn2_includes_prior() {
    let cwd = new_cwd();
    let st = store("s1");
    let r1 = reserved(&st, None, None, "P1", &cwd).unwrap();
    assert!(!r1.replay);
    let a = agent(Box::new(MockBackend::new(vec![result("T1 summary")])), &cwd);
    let out = a.run(&st, &r1).expect("turn1 runs");
    assert_eq!(out.status, "ok");
    assert_eq!(out.branch_id, "b1");

    let snap = st.snapshot().unwrap();
    let turn1 = snap.branches.iter().find(|b| b.branch_id == "b1").unwrap();
    assert_eq!(turn1.lifecycle, Lifecycle::Completed);
    assert_eq!(turn1.rounds.len(), 1);
    assert_eq!(turn1.rounds[0].assistant, "T1 summary");
    assert!(turn1.rounds[0].calls.is_empty());
    assert_eq!(turn1.summary, "T1 summary");
    assert_eq!(turn1.owner, r1.owner);

    // Turn 2 materializes prior user prompt + final assistant response.
    let r2 = reserved(&st, None, None, "P2", &cwd).unwrap();
    assert!(!r2.replay);
    assert_eq!(r2.history.len(), 1);
    assert_eq!(r2.history[0].prompt, "P1");
    assert_eq!(r2.history[0].rounds[0].assistant, "T1 summary");

    let a = agent(Box::new(MockBackend::new(vec![result("T2 summary")])), &cwd);
    let b2 = a.run(&st, &r2).expect("turn2 runs");
    assert_eq!(b2.status, "ok");
}

#[test]
fn multi_tool_turn_persists_call_ids_results_and_turn2_replays_roles() {
    let cwd = new_cwd();
    let st = store("s2");
    let round1 = LlmResult {
        text: "reading".to_string(),
        calls: vec![
            ToolCall {
                id: "call-1".into(),
                name: "write_file".into(),
                args_json: r#"{"path":"f.txt","content":"x"}"#.into(),
            },
            ToolCall {
                id: "call-2".into(),
                name: "list_directory".into(),
                args_json: r#"{"path":"."}"#.into(),
            },
        ],
        finish_reason: Some(FinishReason::ToolCall),
    };
    let r1 = reserved(&st, None, None, "P1", &cwd).unwrap();
    let a = agent(
        Box::new(MockBackend::new(vec![round1, result("done t1")])),
        &cwd,
    );
    let out = a.run(&st, &r1).expect("turn1");
    assert_eq!(out.status, "ok");
    assert_eq!(out.tool_count, 2);

    let snap = st.snapshot().unwrap();
    let b1 = snap.branches.iter().find(|b| b.branch_id == "b1").unwrap();
    assert_eq!(b1.rounds.len(), 2);
    assert_eq!(b1.rounds[0].calls.len(), 2);
    assert_eq!(b1.rounds[0].calls[0].id, "call-1");
    assert_eq!(b1.rounds[1].assistant, "done t1");
    assert!(b1.rounds[1].calls.is_empty());

    let r2 = reserved(&st, None, None, "P2", &cwd).unwrap();
    let h = &r2.history[0];
    assert_eq!(h.prompt, "P1");
    assert_eq!(h.rounds[0].calls[0].id, "call-1");
    assert!(h.rounds[0].calls[0].result.contains("wrote 1 bytes"));
    assert_eq!(h.rounds[0].calls[1].id, "call-2");
    let parts = replay_parts(h);
    assert!(parts.contains("call-1"));
    assert!(parts.contains("call-2"));
}

#[test]
fn same_prompt_replay_does_no_network() {
    let cwd = new_cwd();
    let st = store("s3");
    let r1 = reserved(&st, None, None, "P1", &cwd).unwrap();
    let a = agent(Box::new(MockBackend::new(vec![result("T1 summary")])), &cwd);
    a.run(&st, &r1).expect("turn1");

    let backend = MockBackend::new(Vec::new());
    let r = reserved(&st, Some(1), None, "P1", &cwd).unwrap();
    assert!(r.replay, "same completed prompt must replay");
    let a = agent(Box::new(backend), &cwd);
    let out = a.run(&st, &r).expect("replay");
    assert_eq!(out.status, "ok", "replayed run reports ok");
    assert_eq!(a.model_calls(), 0, "no network on replay");
    assert!(out.replayed);
}

#[test]
fn changed_prompt_creates_branch_and_branch_continuation_excludes_sibling() {
    let cwd = new_cwd();
    let st = store("s4");
    let r1 = reserved(&st, None, None, "P1", &cwd).unwrap();
    let a = agent(Box::new(MockBackend::new(vec![result("P1 answer")])), &cwd);
    a.run(&st, &r1).expect("turn1 A");

    // Different prompt at turn 1 -> a new branch with explicit parent lineage.
    let r1b = reserved(&st, Some(1), None, "P1b", &cwd).unwrap();
    assert!(!r1b.replay);
    assert_ne!(r1b.branch_id, "b1");
    let a = agent(Box::new(MockBackend::new(vec![result("P1b answer")])), &cwd);
    a.run(&st, &r1b).expect("turn1 branch");

    // Continuation (turn 2) continues from the P1b branch and excludes the P1 sibling.
    let r2 = reserved(&st, None, None, "P3", &cwd).unwrap();
    assert!(!r2.replay);
    assert_eq!(r2.history.len(), 1, "only the branch lineage, no sibling");
    assert_eq!(r2.history[0].prompt, "P1b");
    let backend = MockBackend::new(vec![final_round("P3 done")]);
    let a = agent(Box::new(backend), &cwd);
    let out = a.run(&st, &r2).expect("continuation");
    assert_eq!(out.status, "ok");
}

#[test]
fn active_pending_reservation_cannot_be_executed_and_stale_lease_reclaims() {
    let cwd = new_cwd();
    // Two independent stores = two processes.
    let st1 = store("s5a");
    let st2 = store("s5a");
    let r1 = reserved(&st1, None, None, "P1", &cwd).unwrap();
    assert!(!r1.replay);
    // Active pending with a live lease cannot be executed by another process.
    match reserved(&st2, Some(1), None, "P1", &cwd) {
        Err(StoreError::Busy(_)) => {}
        other => panic!("expected Busy, got {other:?}"),
    }

    // The owner (process 1) runs its own reservation once.
    let a = agent(Box::new(MockBackend::new(vec![result("done")])), &cwd);
    a.run(&st1, &r1).expect("finish turn1");

    // Process 1 open a *pending* turn 2, then its lease goes stale.
    let r2 = reserved(&st1, Some(2), None, "P2", &cwd).unwrap();
    assert!(!r2.replay);
    expire_pending(&st1);

    // Process 2 reclaims the stale pending turn 2: it is not a replay.
    let reclaim = reserved(&st2, Some(2), None, "P2", &cwd).unwrap();
    assert!(
        !reclaim.replay,
        "stale pending must be resumed, not replayed"
    );
    let a = agent(Box::new(MockBackend::new(vec![result("redone")])), &cwd);
    a.run(&st2, &reclaim).expect("reclaimed turn runs again");
}

#[test]
fn atomic_cwd_pin_conflicts() {
    let cwd = new_cwd();
    let other = new_cwd();
    let st = store("s6");
    let _ = reserved(&st, None, None, "P1", &cwd).unwrap();
    match reserved(&st, None, None, "P2", &other) {
        Err(StoreError::Invalid(m)) if m.contains("pinned") => {}
        other => panic!("expected cwd-mismatch, got {other:?}"),
    }
}

#[test]
fn syntactic_and_semantic_corruption_is_an_error() {
    let cwd = new_cwd();
    let st = store("s7");
    let _ = reserved(&st, None, None, "P1", &cwd).unwrap();
    // Syntactic corruption is an error, not a fallback.
    std::fs::write(st.session_dir.join("session.manifest.json"), b"not json {").unwrap();
    let reopened = store("s7");
    match reopened.start_request(None, None, "P1", &cwd) {
        Err(StoreError::Corrupt(_)) => {}
        other => panic!("expected Corrupt for garbage, got {other:?}"),
    }

    // Semantic corruption: a mismatched digest must be rejected on load.
    let st3 = store("s8");
    let _ = reserved(&st3, None, None, "Q1", &cwd).unwrap();
    let base = shared_root();
    let dir = base.join("code-rs-sessions/s8");
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("session.manifest.json")).unwrap()).unwrap();
    let segment = manifest["current"]["segment"].as_str().unwrap();
    let path = dir.join(segment);
    let mut bytes = std::fs::read(&path).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    std::fs::write(path, bytes).unwrap();
    let cw = cwd.clone();
    let reopened = store("s8");
    match reopened.start_request(None, None, "X", &cw) {
        Err(StoreError::Corrupt(_)) => {}
        other => panic!("expected Corrupt for bad digest, got {other:?}"),
    }
}

#[test]
fn u32_max_turn_state_cannot_panic() {
    let cwd = new_cwd();
    let root = shared_root();
    let dir = root.join("code-rs-sessions/s9max");
    std::fs::create_dir_all(&dir).unwrap();
    let state = SessionState {
        version: 2,
        session_id: "s9max".into(),
        cwd: Some(cwd.canonicalize().unwrap().to_string_lossy().to_string()),
        cwd_dev: workspace_identity(&cwd).0,
        cwd_ino: workspace_identity(&cwd).1,
        branches: vec![branch(u32::MAX, 1, "b1", "P1", Lifecycle::Completed)],
        next_branch_seq: 1,
    };
    std::fs::write(
        dir.join("session.json"),
        serde_json::to_vec(&state).unwrap(),
    )
    .unwrap();
    let st2 = SessionStore::load(&SessionId::parse("s9max").unwrap()).unwrap();
    match st2.start_request(None, None, "P2", &cwd) {
        Err(StoreError::Corrupt(_)) | Err(StoreError::Invalid(_)) => {}
        other => panic!("a turn-max root must be rejected, got {other:?}"),
    }
}

/// A branch whose parent is not completed (pending/failed) is corruption: a pending or
/// failed prompt can never be continued by a child.
#[test]
fn child_of_failed_parent_is_corrupt() {
    let cwd = new_cwd();
    let root = shared_root();
    let dir = root.join("code-rs-sessions/failedparent");
    std::fs::create_dir_all(&dir).unwrap();
    // b1 completed at turn 1, b2 failed at turn 2, b3 child of b2 at turn 3.
    let p1 = branch(1, 1, "b1", "P1", Lifecycle::Completed);
    let p2 = branch(2, 1, "b2", "P2", Lifecycle::Failed);
    let ch = {
        let mut b = branch(3, 1, "b3", "P3", Lifecycle::Completed);
        b.parent_branch = Some("b2".into());
        b.parent_turn = 2;
        b.parent_attempt = 1;
        b
    };
    let state = SessionState {
        version: 2,
        session_id: "failedparent".into(),
        cwd: Some(cwd.canonicalize().unwrap().to_string_lossy().to_string()),
        cwd_dev: workspace_identity(&cwd).0,
        cwd_ino: workspace_identity(&cwd).1,
        branches: vec![p1, p2, ch],
        next_branch_seq: 3,
    };
    std::fs::write(
        dir.join("session.json"),
        serde_json::to_vec(&state).unwrap(),
    )
    .unwrap();
    let store = SessionStore::load(&SessionId::parse("failedparent").unwrap()).unwrap();
    match store.start_request(None, None, "X", &cwd) {
        Err(StoreError::Corrupt(_)) => {}
        other => panic!("a child of a failed parent must be corrupt, got {other:?}"),
    }
}

/// Session validation must reject a turn-1 child whose parent metadata points at a turn-2
/// branch (parent.turn + 1 != child.turn) as corruption.
#[test]
fn turn1_child_of_turn2_parent_is_corrupt() {
    let cwd = new_cwd();
    let _ = shared_root();
    let root = shared_root();
    {
        let dir = root.join("code-rs-sessions/turn1child");
        let t2: SessionState = SessionState {
            version: 2,
            session_id: "turn1child".into(),
            cwd: Some(cwd.canonicalize().unwrap().to_string_lossy().to_string()),
            cwd_dev: workspace_identity(&cwd).0,
            cwd_ino: workspace_identity(&cwd).1,
            branches: vec![branch(2, 1, "b1", "P1", Lifecycle::Completed)],
            next_branch_seq: 1,
        };
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("session.json"), serde_json::to_vec(&t2).unwrap()).unwrap();
    }
    // A separate session: b1 parent turn 2 with a b1 child at turn 1 referencing it.
    let dir = root.join("code-rs-sessions/turn1child2");
    std::fs::create_dir_all(&dir).unwrap();
    let ch = {
        let mut b = branch(2, 1, "b1", "P1", Lifecycle::Completed);
        b.parent_branch = Some("b2".into());
        b.parent_turn = 1;
        b.parent_attempt = 1;
        b
    };
    let st: SessionState = SessionState {
        version: 2,
        session_id: "turn1child2".into(),
        cwd: Some(cwd.canonicalize().unwrap().to_string_lossy().to_string()),
        cwd_dev: workspace_identity(&cwd).0,
        cwd_ino: workspace_identity(&cwd).1,
        branches: vec![ch],
        next_branch_seq: 2,
    };
    std::fs::write(dir.join("session.json"), serde_json::to_vec(&st).unwrap()).unwrap();
    let store = SessionStore::load(&SessionId::parse("turn1child2").unwrap()).unwrap();
    match store.start_request(None, None, "X", &cwd) {
        Err(StoreError::Corrupt(_)) => {}
        other => panic!("expected Corrupt for the invalid turn-1 child, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn secure_modes() {
    use std::os::unix::fs::PermissionsExt;
    let cwd = new_cwd();
    let st = store("s10");
    let _ = reserved(&st, None, None, "P1", &cwd).unwrap();
    let m = std::fs::metadata(&st.session_dir)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(m, 0o700, "session dir must be 0700");
    let m = std::fs::metadata(st.session_dir.join(".lock"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(m, 0o600, "lock file must be 0600");
    let artifacts = std::fs::read_dir(&st.session_dir)
        .unwrap()
        .map(|entry| entry.expect("read session artifact"))
        .filter(|entry| entry.file_name() != ".lock")
        .collect::<Vec<_>>();
    assert!(
        artifacts
            .iter()
            .any(|entry| entry.file_name() == "session.manifest.json"),
        "manifest artifact exists"
    );
    assert!(
        artifacts
            .iter()
            .any(|entry| entry.file_name().to_string_lossy().starts_with("snapshot-")),
        "snapshot artifact exists"
    );
    assert!(
        artifacts
            .iter()
            .any(|entry| entry.file_name().to_string_lossy().starts_with("segment-")),
        "segment artifact exists"
    );
    for artifact in artifacts {
        assert!(
            artifact.file_type().unwrap().is_file(),
            "persisted artifact must be a file"
        );
        let mode = artifact.metadata().unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "persisted artifact must be 0600");
    }
}

/// The session store caps oversized prompts instead of persisting them (the input limit).
#[test]
fn oversized_prompt_is_rejected() {
    use llxprt_code_rs::session::MAX_PROMPT_BYTES;
    let cwd = new_cwd();
    let st = store("s23");
    let huge = "x".repeat(MAX_PROMPT_BYTES + 1);
    match st.start_request(None, None, &huge, &cwd) {
        Err(StoreError::Invalid(m)) if m.contains("prompt exceeds") => {}
        other => panic!("expected prompt limit, got {other:?}"),
    }
}

#[test]
fn length_finish_reason_persists_failed() {
    let cwd = new_cwd();
    let st = store("s11");
    let r1 = reserved(&st, None, None, "P1", &cwd).unwrap();
    let truncated = LlmResult {
        text: "half".to_string(),
        calls: Vec::new(),
        finish_reason: Some(FinishReason::Length),
    };
    let a = agent(Box::new(MockBackend::new(vec![truncated])), &cwd);
    let err = a.run(&st, &r1).expect_err("length must fail");
    assert_eq!(err.key, "finish-reason");
    let snap = st.snapshot().unwrap();
    let b1 = snap.branches.iter().find(|b| b.branch_id == "b1").unwrap();
    assert_eq!(b1.lifecycle, Lifecycle::Failed);
    assert!(b1.error.contains("finish_reason"));
}

#[test]
fn normalized_empty_object_cannot_execute() {
    let cwd = new_cwd();
    let cfg = llxprt_code_rs::tools::ToolConfig {
        ws: llxprt_code_rs::tools::WorkspaceCap::open(&cwd).unwrap(),
        max_output_bytes: 4096,
        shell: llxprt_code_rs::tools::ShellConfig {
            max_shell_output: 4096,
            max_shell_timeout: std::time::Duration::from_secs(5),
            allow_shell: false,
        },
    };
    // SerdesAI normalizes malformed args to {}; {} must fail every tool because
    // every tool (including list_directory) has a required discriminating field.
    let (ok, msg) =
        llxprt_code_rs::tools::execute_tool(&cwd, "list_directory", serde_json::json!({}), &cfg);
    assert!(!ok, "normalized {{}} must fail list_directory: {msg}");
    let (ok, _) =
        llxprt_code_rs::tools::execute_tool(&cwd, "write_file", serde_json::json!({}), &cfg);
    assert!(!ok);
    let (ok, _) =
        llxprt_code_rs::tools::execute_tool(&cwd, "read_file", serde_json::json!({}), &cfg);
    assert!(!ok);
}

#[test]
fn empty_id_fails_before_side_effect() {
    let cwd = new_cwd();
    let st = store("s12");
    let bad = LlmResult {
        text: "".to_string(),
        calls: vec![ToolCall {
            id: "".into(),
            name: "write_file".into(),
            args_json: r#"{"path":"nope.txt","content":"x"}"#.into(),
        }],
        finish_reason: Some(FinishReason::ToolCall),
    };
    let r = reserved(&st, None, None, "P1", &cwd).unwrap();
    let a = agent(Box::new(MockBackend::new(vec![bad])), &cwd);
    let _ = a.run(&st, &r).err();
    assert!(
        !cwd.join("nope.txt").exists(),
        "side effect must not happen"
    );
    let snap = st.snapshot().unwrap();
    assert_eq!(snap.branches[0].lifecycle, Lifecycle::Failed);
}

#[test]
fn duplicate_ids_fail() {
    let cwd = new_cwd();
    let st = store("s13");
    let dup = LlmResult {
        text: "".to_string(),
        calls: vec![
            ToolCall {
                id: "same".into(),
                name: "write_file".into(),
                args_json: r#"{"path":"b.txt","content":"2"}"#.into(),
            },
            ToolCall {
                id: "same".into(),
                name: "write_file".into(),
                args_json: r#"{"path":"b.txt","content":"2"}"#.into(),
            },
        ],
        finish_reason: Some(FinishReason::ToolCall),
    };
    let r = reserved(&st, None, None, "P1", &cwd).unwrap();
    let a = agent(Box::new(MockBackend::new(vec![dup])), &cwd);
    let e = a.run(&st, &r).expect_err("duplicate ids must fail");
    assert!(e.key == "invalid-tool-call", "{e:?}");
    let snap = st.snapshot().unwrap();
    assert_eq!(snap.branches[0].lifecycle, Lifecycle::Failed);
}

#[test]
fn budget_exhaustion_refuses_excess_and_forces_a_summary() {
    let cwd = new_cwd();
    let st = store("s14");
    let r = reserved(&st, None, None, "P1", &cwd).unwrap();
    // 17 tool rounds against a declared 16-call budget: the 17th must be refused,
    // never executed, and the turn must complete through a forced summary. (The
    // default budget is unlimited; caps are opt-in.)
    let mut replies: Vec<LlmResult> = (0..17)
        .map(|i| LlmResult {
            text: String::new(),
            calls: vec![ToolCall {
                id: format!("c{i}"),
                name: "write_file".into(),
                args_json: format!(r#"{{"path":"g{i}.txt","content":"x"}}"#),
            }],
            finish_reason: Some(FinishReason::ToolCall),
        })
        .collect();
    replies.push(LlmResult {
        text: "wrapped up".into(),
        calls: Vec::new(),
        finish_reason: Some(FinishReason::Stop),
    });
    let a = agent(Box::new(MockBackend::new(replies)), &cwd).with_max_tool_calls(Some(16));
    let run = a.run(&st, &r).expect("exhaustion must complete gracefully");
    assert_eq!(run.tool_count, 16, "only the fitting calls execute");
    assert!(run.budget_exhausted, "the envelope flags exhaustion");
    assert_eq!(run.declared_tool_calls, Some(16));
    assert_eq!(run.summary, "wrapped up", "the forced summary wins");
    assert!(run.status == "ok");
    assert!(!cwd.join("g16.txt").exists(), "the refused call never runs");
    assert!(cwd.join("g15.txt").exists(), "the last fitting call ran");
}

#[test]
fn failed_state_persists_error() {
    let cwd = new_cwd();
    let st = store("s15");
    let r = reserved(&st, None, None, "P1", &cwd).unwrap();
    let bad = LlmResult {
        text: "".to_string(),
        calls: Vec::new(),
        finish_reason: Some(FinishReason::ContentFilter),
    };
    let a = agent(Box::new(MockBackend::new(vec![bad])), &cwd);
    let e = a.run(&st, &r).expect_err("content_filter must fail");
    assert!(e.message.contains("content_filter"));
    let snap = st.snapshot().unwrap();
    assert_eq!(snap.branches[0].lifecycle, Lifecycle::Failed);
}

// --- agent budget and aggregate-cap behaviour (deterministic mocks) ---

/// A context budget so small that even the first request is over budget must refuse up
/// front, never making a single model call.
#[test]
fn first_turn_tiny_context_makes_zero_model_calls() {
    let cwd = new_cwd();
    let st = store("sctx1");
    let r = reserved(&st, None, None, "P1", &cwd).unwrap();
    let a = agent(Box::new(MockBackend::new(vec![result("x")])), &cwd).with_context_limit(Some(1));
    let e = a
        .run(&st, &r)
        .expect_err("a 1-token budget must refuse the first request");
    assert_eq!(e.key, "context-limit");
    assert_eq!(
        a.model_calls(),
        0,
        "no model call when the first request is over budget"
    );
    let snap = st.snapshot().unwrap();
    assert_eq!(snap.branches[0].lifecycle, Lifecycle::Failed);
}

/// A later round whose raw tool output would blow the context budget no longer stops the
/// attempt: bulk results are digested pre-entry, so the replayed request carries a compact
/// CTXDIGEST record instead of the payload, and the full bytes move to the spine and vault.
#[test]
fn later_round_context_overflow_stops_before_next_call() {
    let cwd = new_cwd();
    let st = store("sctx2");
    let r = reserved(&st, None, None, "P1", &cwd).unwrap();
    // The first request (system + prompt + tool schemas) fits a 600 KiB budget; its
    // read_file round would materialize ~1 MiB of raw tool output into the next request.
    // Compacting that result pre-entry keeps the next request inside the budget, so the
    // attempt completes instead of being refused by the context limit.
    let payload_len = 1024 * 1024;
    // read_file frames the window as "[0..N of N bytes]\n" before the content, so the
    // digested result is the payload plus that header; the digest reports the framed size.
    let framed_len = payload_len + format!("[0..{payload_len} of {payload_len} bytes]\n").len();
    std::fs::write(cwd.join("big.txt"), "y".repeat(payload_len)).unwrap();
    let round = LlmResult {
        text: "reading".to_string(),
        calls: vec![ToolCall {
            id: "c0".into(),
            name: "read_file".into(),
            args_json: r#"{"path":"big.txt"}"#.into(),
        }],
        finish_reason: Some(FinishReason::ToolCall),
    };
    let a = agent(
        Box::new(MockBackend::new(vec![round, result("done")])),
        &cwd,
    )
    .with_context_limit(Some(200_000));
    let run = a.run(&st, &r).expect("the digested replay fits the budget");
    assert_eq!(run.status, "ok", "the attempt completes");
    assert_eq!(run.tool_count, 1);
    assert!(!run.budget_exhausted, "nothing was cut short");
    assert_eq!(a.model_calls(), 2, "the follow-up request is still sent");

    // The bulk content was contained rather than replayed raw.
    let snapshot = st.snapshot().unwrap();
    let branch = snapshot
        .branches
        .iter()
        .find(|branch| branch.branch_id == r.branch_id)
        .unwrap();
    let retained = &branch.rounds[0].calls[0].result;
    assert!(
        retained.starts_with("CTXDIGEST v1 tool=read_file "),
        "the bulk result is retained as a digest record: {retained}"
    );
    assert!(
        retained.contains(&format!("bytes={framed_len}")),
        "the record still reports the full payload size: {retained}"
    );
    assert!(
        retained.len() < 4096,
        "the retained record stays bounded: {retained}"
    );
    let handle = digest_handle(retained);
    assert!(
        handle.starts_with("content-") && handle.len() == "content-".len() + 16,
        "the record carries a content digest handle, not a vault slot: {handle}"
    );
    assert!(
        context_artifact(&st, "sanitized").len() >= payload_len,
        "the sanitized spine holds the payload bytes"
    );
    assert!(
        context_artifact(&st, "vault").len() >= payload_len,
        "the vault holds the full payload bytes"
    );
}

/// Two rounds each under the per-turn assistant cap are still bounded together: the total
/// assistant bytes across the whole attempt count, and the overflowing round's side effect
/// never runs.
#[test]
fn aggregate_assistant_bytes_across_rounds_rejected() {
    use llxprt_code_rs::agent::MAX_TURN_ASSISTANT_BYTES;
    let cwd = new_cwd();
    let st = store("sagg1");
    let r = reserved(&st, None, None, "P1", &cwd).unwrap();
    let big_tool = |id: &str, path: &str, ch: char, n: usize| LlmResult {
        text: ch.to_string().repeat(n),
        calls: vec![ToolCall {
            id: id.to_string(),
            name: "write_file".into(),
            args_json: format!(r#"{{"path":"{path}","content":"x"}}"#),
        }],
        finish_reason: Some(FinishReason::ToolCall),
    };
    let rounds = vec![
        big_tool("ca", "agg-a.txt", 'a', MAX_TURN_ASSISTANT_BYTES - 64),
        big_tool("cb", "agg-b.txt", 'b', MAX_TURN_ASSISTANT_BYTES - 64),
        result("done"),
    ];
    let a = agent(Box::new(MockBackend::new(rounds)), &cwd);
    let e = a
        .run(&st, &r)
        .expect_err("the sum across rounds must be capped");
    assert_eq!(e.key, "turn-budget", "{e:?}");
    assert!(cwd.join("agg-a.txt").exists());
    assert!(!cwd.join("agg-b.txt").exists());
    let snap = st.snapshot().unwrap();
    assert_eq!(snap.branches[0].lifecycle, Lifecycle::Failed);
}

/// Raw tool-call argument bytes also aggregate across the whole attempt: the total of both
/// rounds counts, and the overflowing round's side effect never runs.
#[test]
fn aggregate_args_across_rounds_rejected() {
    use llxprt_code_rs::agent::MAX_TURN_ARGS_BYTES;
    let cwd = new_cwd();
    let st = store("sagg2");
    let r = reserved(&st, None, None, "P1", &cwd).unwrap();
    let arg_round = |id: &str, path: &str, ch: char| LlmResult {
        text: "working".to_string(),
        calls: vec![ToolCall {
            id: id.to_string(),
            name: "write_file".into(),
            args_json: format!(
                r#"{{"path":"{path}","content":"{}"}}"#,
                ch.to_string().repeat(MAX_TURN_ARGS_BYTES - 128)
            ),
        }],
        finish_reason: Some(FinishReason::ToolCall),
    };
    let rounds = vec![
        arg_round("ca", "args-a.txt", 'a'),
        arg_round("cb", "args-b.txt", 'b'),
        result("done"),
    ];
    let a = agent(Box::new(MockBackend::new(rounds)), &cwd);
    let e = a
        .run(&st, &r)
        .expect_err("the sum of raw args across rounds must be capped");
    assert_eq!(e.key, "turn-budget", "{e:?}");
    assert!(cwd.join("args-a.txt").exists());
    assert!(!cwd.join("args-b.txt").exists());
}

/// Tool outputs consume one shared turn budget. Later calls receive only the remaining bytes,
/// so neither the next model request nor the persisted round can contain an oversized
/// aggregate. Bulk results are then compacted pre-entry: the persisted round holds one
/// bounded CTXDIGEST record per call while the full bytes live in the vault.
#[test]
fn multiple_tool_calls_share_remaining_output_budget() {
    use llxprt_code_rs::agent::MAX_TURN_OUTPUT_BYTES;

    let cwd = new_cwd();
    let payload_len = 1024 * 1024;
    // read_file frames the window as "[0..N of N bytes]\n" before the content, so the
    // digested result is the payload plus that header; the digest reports the framed size.
    let framed_len = payload_len + format!("[0..{payload_len} of {payload_len} bytes]\n").len();
    std::fs::write(cwd.join("megabyte.txt"), "z".repeat(payload_len)).unwrap();
    let st = store("sagg-output");
    let reserved = reserved(&st, None, None, "P1", &cwd).unwrap();
    let calls = (0..16)
        .map(|index| ToolCall {
            id: format!("read-{index}"),
            name: "read_file".into(),
            args_json: r#"{"path":"megabyte.txt"}"#.into(),
        })
        .collect();
    let tool_round = LlmResult {
        text: "reading".into(),
        calls,
        finish_reason: Some(FinishReason::ToolCall),
    };
    let agent = agent(
        Box::new(MockBackend::new(vec![tool_round, result("done")])),
        &cwd,
    );

    agent.run(&st, &reserved).expect("bounded turn succeeds");
    let snapshot = st.snapshot().unwrap();
    let branch = snapshot
        .branches
        .iter()
        .find(|branch| branch.branch_id == reserved.branch_id)
        .unwrap();
    let retained_calls = &branch.rounds[0].calls;
    assert_eq!(retained_calls.len(), 16, "every call is retained");
    for call in retained_calls {
        assert!(
            call.result.starts_with("CTXDIGEST v1 tool=read_file "),
            "each bulk result is retained as a digest record: {}",
            call.result
        );
    }
    assert!(
        retained_calls[0]
            .result
            .contains(&format!("bytes={framed_len}")),
        "the record reports the payload the shared budget left for one call: {}",
        retained_calls[0].result
    );
    assert!(
        retained_calls
            .iter()
            .all(|call| call.result == retained_calls[0].result),
        "identical clipped outputs render identical digest records"
    );
    let output_bytes: usize = retained_calls.iter().map(|call| call.result.len()).sum();
    assert_eq!(
        output_bytes, 1632,
        "16 bounded digest records are retained, not 16 MiB of raw output"
    );
    assert!(
        output_bytes < MAX_TURN_OUTPUT_BYTES,
        "the retained aggregate stays inside the shared output budget"
    );
    let handle = digest_handle(&retained_calls[0].result);
    assert!(
        handle.starts_with("content-") && handle.len() == "content-".len() + 16,
        "the record carries a content digest handle, not a vault slot: {handle}"
    );
    assert!(
        context_artifact(&st, "sanitized").len() >= payload_len,
        "the sanitized spine holds the payload bytes"
    );
    assert!(
        context_artifact(&st, "vault").len() >= payload_len,
        "the vault holds the full payload bytes"
    );
}

/// A single search that could render more than the whole turn budget is clipped to that
/// budget and then compacted to one bounded CTXDIGEST record before it reaches the next
/// model request or the persisted session record; the clipped bytes go to the spine and
/// vault.
#[test]
fn oversized_search_output_is_bounded_before_retention() {
    use llxprt_code_rs::agent::MAX_TURN_OUTPUT_BYTES;

    let cwd = new_cwd();
    let line = format!("needle {}\n", "x".repeat(32 * 1024));
    let file = line.repeat(31);
    for index in 0..20 {
        std::fs::write(cwd.join(format!("search-{index}.txt")), &file).unwrap();
    }
    let st = store("sagg-search-output");
    let reserved = reserved(&st, None, None, "P1", &cwd).unwrap();
    let tool_round = LlmResult {
        text: "searching".into(),
        calls: vec![ToolCall {
            id: "search-1".into(),
            name: "search_file_content".into(),
            args_json: r#"{"pattern":"needle","max_results":2000}"#.into(),
        }],
        finish_reason: Some(FinishReason::ToolCall),
    };
    let agent = agent(
        Box::new(MockBackend::new(vec![tool_round, result("done")])),
        &cwd,
    );

    agent.run(&st, &reserved).expect("bounded search succeeds");
    let snapshot = st.snapshot().unwrap();
    let branch = snapshot
        .branches
        .iter()
        .find(|branch| branch.branch_id == reserved.branch_id)
        .unwrap();
    let retained = &branch.rounds[0].calls[0].result;
    assert!(
        retained.starts_with("CTXDIGEST v1 tool=search_file_content "),
        "the oversized result is retained as a digest record: {retained}"
    );
    assert!(
        retained.contains(&format!("bytes={MAX_TURN_OUTPUT_BYTES}")),
        "the record reports the clipped size the budget allowed: {retained}"
    );
    assert_eq!(
        retained.len(),
        113,
        "one bounded digest record is retained: {retained}"
    );
    assert!(retained.len() < MAX_TURN_OUTPUT_BYTES);
    let handle = digest_handle(retained);
    assert!(
        handle.starts_with("content-") && handle.len() == "content-".len() + 16,
        "the record carries a content digest handle, not a vault slot: {handle}"
    );
    assert!(
        context_artifact(&st, "sanitized").len() >= MAX_TURN_OUTPUT_BYTES,
        "the sanitized spine holds the payload bytes"
    );
    assert!(
        context_artifact(&st, "vault").len() >= MAX_TURN_OUTPUT_BYTES,
        "the vault holds the full payload bytes"
    );
}

/// A request timeout at or above the lease minus the safety margin is refused up front: a
/// single request must always fit inside one lease.
#[test]
fn timeout_at_lease_boundary_is_rejected() {
    use llxprt_code_rs::agent::{validate_timeout, TIMEOUT_LEASE_MARGIN_SECONDS};
    use llxprt_code_rs::session::LEASE_SECONDS;
    let boundary = LEASE_SECONDS - TIMEOUT_LEASE_MARGIN_SECONDS;
    assert!(validate_timeout(Some(std::time::Duration::from_secs(LEASE_SECONDS))).is_err());
    assert!(validate_timeout(Some(std::time::Duration::from_secs(boundary))).is_err());
    assert!(validate_timeout(Some(std::time::Duration::from_secs(boundary - 1))).is_ok());
    assert!(validate_timeout(None).is_ok());
}

/// An explicit turn may not skip forward past the selected lineage's latest turn; it may only
/// advance by one step at a time.
#[test]
fn fresh_turn_5_after_turn_1_is_rejected_as_a_gap() {
    let cwd = new_cwd();
    let st = store("s22");
    let r1 = reserved(&st, None, None, "P1", &cwd).unwrap();
    let a = agent(Box::new(MockBackend::new(vec![result("T1")])), &cwd);
    a.run(&st, &r1).unwrap();
    match reserved(&st, Some(5), None, "P5", &cwd) {
        Err(StoreError::Invalid(m)) if m.contains("beyond the selected lineage") => {}
        other => panic!("expected a gap rejection for --turn 5, got {other:?}"),
    }
}

// --- lease and overflow regressions ---

/// `checkpoint` atomically extends the lease under the same lock: after a simulated
/// elapsed interval the pending branch has its rounds AND a strictly later lease expiry.
#[test]
fn checkpoint_extends_lease() {
    let cwd = new_cwd();
    let st = store("ckpt1");
    let r = reserved(&st, None, None, "P1", &cwd).unwrap();
    let before = on_disk_lease(&st, &r);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let rounds = vec![RoundRecord {
        assistant: "working".into(),
        calls: vec![ToolCallRecord {
            id: "checkpoint-call".into(),
            name: "read_file".into(),
            args: r#"{"path":"checkpoint.txt"}"#.into(),
            ok: true,
            refused: false,
            result: "checkpoint result".into(),
        }],
    }];
    st.checkpoint(&r, &rounds).unwrap();
    let after = on_disk_lease(&st, &r);
    assert!(
        after > before,
        "checkpoint must extend the lease: before {before} after {after}"
    );
    let snap = st.snapshot().unwrap();
    let b = snap
        .branches
        .iter()
        .find(|b| b.branch_id == r.branch_id)
        .unwrap();
    assert_eq!(b.lifecycle, Lifecycle::Pending, "checkpoint leaves pending");
    assert_eq!(b.rounds.len(), 1);
    assert_eq!(b.owner, r.owner);
}

#[test]
fn empty_suffix_checkpoint_extends_persisted_lease() {
    let cwd = new_cwd();
    let st = store("ckpt-empty");
    let r = reserved(&st, None, None, "P1", &cwd).unwrap();
    let before = on_disk_lease(&st, &r);
    std::thread::sleep(std::time::Duration::from_millis(1100));

    st.checkpoint(&r, &[]).unwrap();

    let after = on_disk_lease(&st, &r);
    assert!(
        after > before,
        "empty checkpoint must renew the persisted lease"
    );
}

/// The second model call of a tool round observes a freshly renewed lease after a
/// simulated elapsed interval: the reserved lease is recorded, a whole second passes, and
/// the turn's second pre-request renew extends it strictly (the agent renews right
/// before every post-tool backend call), and the completion succeeds.
#[test]
fn second_model_call_observes_renewed_lease_after_elapsed_interval() {
    let cwd = new_cwd();
    let st = store("ckpt2");
    let r = reserved(&st, Some(1), None, "P1", &cwd).unwrap();
    let before = on_disk_lease(&st, &r);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let tool = LlmResult {
        text: "next".to_string(),
        calls: vec![ToolCall {
            id: "c1".into(),
            name: "write_file".into(),
            args_json: r#"{"path":"lease.txt","content":"x"}"#.into(),
        }],
        finish_reason: Some(FinishReason::ToolCall),
    };
    let observed = std::sync::Arc::new(Mutex::new(None));
    let observed_in_backend = observed.clone();
    let session_dir = st.session_dir.clone();
    let branch_id = r.branch_id.clone();
    let backend = MockBackend::new(vec![tool, result("done")]).with_observer(move |call| {
        if call == 2 {
            let state = read_current_state(&session_dir);
            let lease = state
                .branches
                .iter()
                .find(|branch| branch.branch_id == branch_id)
                .unwrap()
                .lease_expiry;
            *observed_in_backend.lock().unwrap() = Some(lease);
        }
    });
    let a = agent(Box::new(backend), &cwd);
    a.run(&st, &r).expect("turn with a second model call");
    let after_first = observed
        .lock()
        .unwrap()
        .expect("the second backend call observed the persisted lease");
    assert!(
        after_first > before,
        "the post-tool pre-request renew must extend the lease: before {before} after {after_first}"
    );
    let snap = st.snapshot().unwrap();
    let b = snap
        .branches
        .iter()
        .find(|b| b.branch_id == r.branch_id)
        .unwrap();
    assert_eq!(b.lifecycle, Lifecycle::Completed);
    assert_eq!(b.rounds.len(), 2);
}

/// Checked arithmetic: a fork that would overflow the attempt counter returns a typed
/// input error, never a panic.
#[test]
fn attempt_overflow_is_a_typed_error() {
    let cwd = new_cwd();
    let root = shared_root();
    let dir = root.join("code-rs-sessions/attemptmax");
    std::fs::create_dir_all(&dir).unwrap();
    let state = SessionState {
        version: 2,
        session_id: "attemptmax".into(),
        cwd: Some(cwd.canonicalize().unwrap().to_string_lossy().to_string()),
        cwd_dev: workspace_identity(&cwd).0,
        cwd_ino: workspace_identity(&cwd).1,
        branches: vec![
            branch(1, 1, "b1", "P1", Lifecycle::Completed),
            branch(1, u32::MAX, "b2", "P2", Lifecycle::Completed),
        ],
        next_branch_seq: 2,
    };
    std::fs::write(
        dir.join("session.json"),
        serde_json::to_vec(&state).unwrap(),
    )
    .unwrap();
    let store = SessionStore::load(&SessionId::parse("attemptmax").unwrap()).unwrap();
    match store.start_request(Some(1), None, "fork-attempt", &cwd) {
        Err(StoreError::Invalid(m)) if m.contains("attempt overflow") => {}
        other => panic!("attempt overflow must be a typed Invalid error, got {other:?}"),
    }
    let snap = store.snapshot().unwrap();
    assert_eq!(snap.branches.len(), 2, "no branch may be added on overflow");
}

/// Checked arithmetic: next_branch_seq at u64::MAX with a live completed branch makes a
/// fork return a typed sequence-overflow error, never a panic.
#[test]
fn branch_seq_overflow_is_a_typed_error() {
    let cwd = new_cwd();
    let root = shared_root();
    let dir = root.join("code-rs-sessions/seqmax");
    std::fs::create_dir_all(&dir).unwrap();
    let state = SessionState {
        version: 2,
        session_id: "seqmax".into(),
        cwd: Some(cwd.canonicalize().unwrap().to_string_lossy().to_string()),
        cwd_dev: workspace_identity(&cwd).0,
        cwd_ino: workspace_identity(&cwd).1,
        branches: vec![branch(1, 1, "b1", "P1", Lifecycle::Completed)],
        next_branch_seq: u64::MAX,
    };
    std::fs::write(
        dir.join("session.json"),
        serde_json::to_vec(&state).unwrap(),
    )
    .unwrap();
    let store = SessionStore::load(&SessionId::parse("seqmax").unwrap()).unwrap();
    match store.start_request(Some(1), None, "fork-seq", &cwd) {
        Err(StoreError::Invalid(m)) if m.contains("overflow") => {}
        other => panic!("branch sequence overflow must be typed, got {other:?}"),
    }
    let snap = store.snapshot().unwrap();
    assert_eq!(snap.branches.len(), 1, "no branch may be added on overflow");
}

fn read_current_state(session_dir: &std::path::Path) -> SessionState {
    let id = session_dir.file_name().unwrap().to_str().unwrap();
    SessionStore::load_at(&SessionId::parse(id).unwrap(), &shared_root())
        .unwrap()
        .snapshot()
        .unwrap()
}

fn write_current_state(session_dir: &std::path::Path, state: &SessionState) {
    let id = session_dir.file_name().unwrap().to_str().unwrap();
    SessionStore::load_at(&SessionId::parse(id).unwrap(), &shared_root())
        .unwrap()
        .replace_snapshot(state)
        .unwrap();
}

/// The current lease a reservation holds on disk.
fn on_disk_lease(store: &SessionStore, r: &ReservedRequest) -> u64 {
    let id = SessionId::parse(&store.session_id).unwrap();
    SessionStore::load_at(&id, &shared_root())
        .unwrap()
        .snapshot()
        .unwrap()
        .branches
        .iter()
        .find(|b| b.branch_id == r.branch_id)
        .unwrap()
        .lease_expiry
}

fn final_round(text: &str) -> LlmResult {
    result(text)
}

fn replay_parts(h: &HistoryTurn) -> String {
    let mut out = String::new();
    for r in &h.rounds {
        out.push_str(&serde_json::to_string(r).unwrap());
    }
    out
}

fn branch(turn: u32, attempt: u32, id: &str, prompt: &str, lifecycle: Lifecycle) -> BranchRecord {
    let (rounds, summary, error, owner) = match lifecycle {
        Lifecycle::Pending => (
            Vec::new(),
            String::new(),
            String::new(),
            "owner".to_string(),
        ),
        Lifecycle::Completed => (
            vec![RoundRecord {
                assistant: "done".to_string(),
                calls: Vec::new(),
            }],
            "done".to_string(),
            String::new(),
            String::new(),
        ),
        Lifecycle::Failed => (
            Vec::new(),
            String::new(),
            "failed".to_string(),
            String::new(),
        ),
    };
    // A pending branch carries its lease; a terminal branch releases it.
    let (reserved_at, lease_expiry) = match lifecycle {
        Lifecycle::Pending => (1, 2),
        Lifecycle::Completed | Lifecycle::Failed => (0, 0),
    };
    BranchRecord {
        branch_id: id.to_string(),
        turn,
        attempt,
        parent_branch: None,
        parent_turn: 0,
        parent_attempt: 0,
        prompt: prompt.to_string(),
        digest: llxprt_code_rs::agent::prompt_digest(prompt),
        lifecycle,
        rounds,
        summary,
        error,
        owner,
        reserved_at,
        lease_expiry,
    }
}

/// Directly expire every pending lease on disk (leave owner in place: a stale lease is
/// what identifies a recoverable reservation).
fn expire_pending(store: &SessionStore) {
    let mut state = read_current_state(&store.session_dir);
    for branch in &mut state.branches {
        if branch.lifecycle == Lifecycle::Pending {
            branch.reserved_at = 1;
            branch.lease_expiry = 2;
        }
    }
    write_current_state(&store.session_dir, &state);
}
