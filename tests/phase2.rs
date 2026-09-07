//! Phase 2 tests: versioned session storage, branch/turn/replay/lease/concurrency
//! semantics, full independent-process history continuation, and agent protocol
//! validation, all driven through a mock [`ChatBackend`] — no network.
//!
//! `cargo test` runs this binary's tests in one process with many threads, so the
//! daemon config dir is set **once** to a shared per-process root and every test uses a
//! unique session id under it. No test mutates the env afterwards.

use llxprt_code_rs::adapter::{ChatBackend, LlmResult, LlmUsage, ToolCall};
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
        usage: LlmUsage::default(),
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

/// A scripted backend modeled on [`MockBackend`] that additionally clones every incoming
/// model request (per backend call) into a shared collection, so a test can assert
/// exactly what the next provider request carried (#66 live-bytes evidence).
struct CapturingBackend {
    inner: MockBackend,
    captured: std::sync::Arc<Mutex<Vec<Vec<serdes_ai::core::ModelRequest>>>>,
}

impl CapturingBackend {
    fn new(replies: Vec<LlmResult>) -> Self {
        CapturingBackend {
            inner: MockBackend::new(replies),
            captured: std::sync::Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// A clone of the shared capture handle, taken before the backend is moved into the
    /// agent so the test can read every request the agent sent after the run.
    fn captured_handle(&self) -> std::sync::Arc<Mutex<Vec<Vec<serdes_ai::core::ModelRequest>>>> {
        self.captured.clone()
    }
}

impl ChatBackend for CapturingBackend {
    fn request(
        &self,
        requests: &[serdes_ai::core::ModelRequest],
        _tools: &[ToolSpec],
    ) -> Result<LlmResult, String> {
        self.captured.lock().unwrap().push(requests.to_vec());
        self.inner.request(requests, _tools)
    }

    fn request_calls(&self) -> usize {
        self.inner.request_calls()
    }
}

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

/// Reads one published `context/` artifact of a session. Post-#137 every
/// publication lands as a committed generation directory.
fn context_artifact(st: &SessionStore, name: &str) -> Vec<u8> {
    std::fs::read(st.session_dir.join("context/committed").join(name))
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
        usage: LlmUsage::default(),
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
    let truncated = |text: &str| LlmResult {
        usage: LlmUsage::default(),
        text: text.to_string(),
        calls: Vec::new(),
        finish_reason: Some(FinishReason::Length),
    };
    // Both completions truncate, so the single re-issue (issue 153) is exhausted and the
    // run still fails terminally with the typed finish-reason error.
    let a = agent(
        Box::new(MockBackend::new(vec![
            truncated("half"),
            truncated("half again"),
        ])),
        &cwd,
    );
    let err = a.run(&st, &r1).expect_err("length must fail");
    assert_eq!(err.key, "finish-reason");
    assert_eq!(
        err.terminal_outcome,
        Some(llxprt_code_rs::agent::TRUNCATED_OUTPUT_RETRIED_KEY)
    );
    let snap = st.snapshot().unwrap();
    let b1 = snap.branches.iter().find(|b| b.branch_id == "b1").unwrap();
    assert_eq!(b1.lifecycle, Lifecycle::Failed);
    assert!(b1.error.contains("finish_reason"));
}

/// A truncation after tool work keeps the immediate fatal path: no re-issue, so a single
/// truncated reply after a tool round fails without the exhausted-retry verdict.
#[test]
fn length_finish_reason_after_tool_use_fails_immediately() {
    let cwd = new_cwd();
    let st = store("s11-mid");
    let r1 = reserved(&st, None, None, "P1", &cwd).unwrap();
    let tool_round = LlmResult {
        usage: LlmUsage::default(),
        text: String::new(),
        calls: vec![ToolCall {
            id: "c1".to_string(),
            name: "list_directory".to_string(),
            args_json: r#"{"path":"."}"#.to_string(),
        }],
        finish_reason: Some(FinishReason::ToolCall),
    };
    let truncated = LlmResult {
        usage: LlmUsage::default(),
        text: "half".to_string(),
        calls: Vec::new(),
        finish_reason: Some(FinishReason::Length),
    };
    let a = agent(
        Box::new(MockBackend::new(vec![tool_round, truncated])),
        &cwd,
    );
    let err = a.run(&st, &r1).expect_err("mid-work length must fail");
    assert_eq!(err.key, "finish-reason");
    assert_eq!(err.terminal_outcome, None);
    let snap = st.snapshot().unwrap();
    let b1 = snap.branches.iter().find(|b| b.branch_id == "b1").unwrap();
    assert_eq!(b1.lifecycle, Lifecycle::Failed);
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
        usage: LlmUsage::default(),
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
        usage: LlmUsage::default(),
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
            usage: LlmUsage::default(),
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
        usage: LlmUsage::default(),
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
fn budget_cap_reached_by_natural_wrapup_reports_exhausted() {
    // Two fitting calls against a 2-call cap, then the model stops on its own:
    // total == declared, so the envelope must still report exhaustion — the
    // next call would have been refused (#15).
    let cwd = new_cwd();
    let st = store("s15b");
    let r = reserved(&st, None, None, "P", &cwd).unwrap();
    let calls: Vec<LlmResult> = (0..2)
        .map(|i| LlmResult {
            usage: LlmUsage::default(),
            text: String::new(),
            calls: vec![ToolCall {
                id: format!("c{i}"),
                name: "write_file".into(),
                args_json: format!(r#"{{"path":"n{i}.txt","content":"x"}}"#),
            }],
            finish_reason: Some(FinishReason::ToolCall),
        })
        .collect();
    let mut replies = calls;
    replies.push(LlmResult {
        usage: LlmUsage::default(),
        text: "all done".into(),
        calls: Vec::new(),
        finish_reason: Some(FinishReason::Stop),
    });
    let a = agent(Box::new(MockBackend::new(replies)), &cwd).with_max_tool_calls(Some(2));
    let run = a.run(&st, &r).expect("capped run completes");
    assert_eq!(run.tool_count, 2);
    assert_eq!(run.declared_tool_calls, Some(2));
    assert!(
        run.budget_exhausted,
        "ending exactly at the cap is cap termination"
    );
    assert_eq!(run.status, "ok");
}

#[test]
fn natural_wrapup_below_cap_reports_budget_available() {
    let cwd = new_cwd();
    let st = store("s15c");
    let r = reserved(&st, None, None, "P", &cwd).unwrap();
    let call = LlmResult {
        usage: LlmUsage::default(),
        text: String::new(),
        calls: vec![ToolCall {
            id: "c0".into(),
            name: "write_file".into(),
            args_json: r#"{"path":"one.txt","content":"x"}"#.into(),
        }],
        finish_reason: Some(FinishReason::ToolCall),
    };
    let done = LlmResult {
        usage: LlmUsage::default(),
        text: "done".into(),
        calls: Vec::new(),
        finish_reason: Some(FinishReason::Stop),
    };
    let a =
        agent(Box::new(MockBackend::new(vec![call, done])), &cwd).with_max_tool_calls(Some(256));
    let run = a.run(&st, &r).unwrap();
    assert_eq!(run.tool_count, 1);
    assert!(
        !run.budget_exhausted,
        "a run that stops well under the cap is natural wrap-up"
    );
}

#[test]
fn failed_state_persists_error() {
    let cwd = new_cwd();
    let st = store("s15");
    let r = reserved(&st, None, None, "P1", &cwd).unwrap();
    let bad = LlmResult {
        usage: LlmUsage::default(),
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

/// A later round whose LIVE tool output would blow the context budget must fail the attempt
/// before the second model call: #66 releases the sanitized admitted content to the next
/// provider request, so the replayed request carries the ~1 MiB framed read and the
/// conservative pre-send guard refuses it (context-limit) instead of sending it. The
/// persisted side still behaves: the round's retained record is one bounded CTXDIGEST v1
/// record, and the payload lives in the sanitized spine and the vault.
#[test]
fn later_round_context_overflow_stops_before_next_call() {
    let cwd = new_cwd();
    let st = store("sctx2");
    let r = reserved(&st, None, None, "P1", &cwd).unwrap();
    // The first request (system + prompt + tool schemas) fits a 200_000-token budget
    // (600_000-byte heuristic guard); its read_file round would materialize ~1 MiB of
    // LIVE tool output into the next request, which the guard must refuse up front.
    let payload_len = 1024 * 1024;
    // read_file frames the window as "[0..N of N bytes]\n" before the content, so the
    // released result is the payload plus that header; the digest reports the framed size.
    let framed_len = payload_len + format!("[0..{payload_len} of {payload_len} bytes]\n").len();
    std::fs::write(cwd.join("big.txt"), "y".repeat(payload_len)).unwrap();
    let round = LlmResult {
        usage: LlmUsage::default(),
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
    // The live bytes ride the next request, so that request is over the conservative
    // budget and the attempt dies with the context-limit refusal BEFORE any second
    // model call is made.
    let e = a
        .run(&st, &r)
        .expect_err("the live-byte replay must be refused by the context limit");
    assert_eq!(e.key, "context-limit", "{e:?}");
    assert!(
        e.message.contains("after one compaction attempt, still"),
        "the refusal records #82's single pre-send compaction attempt: {e:?}"
    );
    assert_eq!(
        a.model_calls(),
        1,
        "the over-budget second request is never sent"
    );
    let snapshot = st.snapshot().unwrap();
    let branch = snapshot
        .branches
        .iter()
        .find(|branch| branch.branch_id == r.branch_id)
        .unwrap();
    assert_eq!(
        branch.lifecycle,
        Lifecycle::Failed,
        "the refused attempt is terminal, not pending"
    );

    // The bulk content was contained rather than replayed raw.
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
    let first = st
        .context_read_page(0..64, 64)
        .expect("bounded context read-back");
    let replay = st
        .context_read_page(0..64, 64)
        .expect("deterministic context read-back");
    assert_eq!(first.bytes, replay.bytes);
    assert!(first.remaining.is_none());
    assert!(
        !first.bytes.is_empty(),
        "bounded read-back returns stored evidence"
    );
    let raw_present = retained.contains(&"y".repeat(256));
    assert!(
        !raw_present,
        "raw bulk evidence never reaches the transcript"
    );
    let journal = context_artifact(&st, "rewrite-journal.log");
    assert!(!journal.is_empty(), "policy rewrite accounting is durable");
    assert!(!context_artifact(&st, "events.log").is_empty());
    assert!(!context_artifact(&st, "checkpoints").is_empty());
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
        usage: LlmUsage::default(),
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
        usage: LlmUsage::default(),
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

/// Tool outputs consume one shared turn budget, and #66 charges that budget in LIVE
/// bytes: the sanitized admitted content the next provider request carries. Later calls
/// receive only the remaining bytes, so every retained record is still one bounded
/// CTXDIGEST digest, the first fifteen report the full framed size and the last reports
/// the smaller clipped size the budget had left, and the aggregate never exceeds
/// the resolved turn cap. The payload itself lives in the sanitized spine; a vault copy is
/// present only when ingress quarantine requires one.
#[test]
fn multiple_tool_calls_share_remaining_output_budget() {
    use llxprt_code_rs::agent::OutputCaps;

    let cwd = new_cwd();
    let payload_len = 4096;
    // read_file frames the window as "[0..N of N bytes]\n" before the content, so the
    // released result is the payload plus that header; the digest reports the framed size.
    let frame_header = format!("[0..{payload_len} of {payload_len} bytes]\n");
    let framed_len = payload_len + frame_header.len();
    // A small per-test cap exercises the exact same shared LIVE-byte boundary without
    // repeatedly publishing multi-megabyte evidence during this focused unit test.
    let turn_budget = 16 * framed_len - 1;
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
        usage: LlmUsage::default(),
        text: "reading".into(),
        calls,
        finish_reason: Some(FinishReason::ToolCall),
    };
    // The turn budget is the subject here, not the context limit.  A per-test
    // cap makes sixteen framed bulk reads fit in one request while preserving the
    // production bulk threshold and the exact shared-LIVE-byte boundary.
    let agent = agent(
        Box::new(MockBackend::new(vec![tool_round, result("done")])),
        &cwd,
    )
    .with_context_limit(Some(turn_budget as u64))
    .with_output_caps(OutputCaps {
        turn: turn_budget,
        ..OutputCaps::default()
    });

    agent.run(&st, &reserved).expect("bounded turn succeeds");
    let snapshot = st.snapshot().unwrap();
    let branch = snapshot
        .branches
        .iter()
        .find(|branch| branch.branch_id == reserved.branch_id)
        .unwrap();
    let retained_calls = &branch.rounds[0].calls;
    assert_eq!(
        retained_calls.len(),
        16,
        "every call executes and is retained"
    );
    for call in retained_calls {
        assert!(
            call.result.starts_with("CTXDIGEST v1 tool=read_file "),
            "each bulk result is retained as a digest record: {}",
            call.result
        );
    }

    // The first fifteen reads fit the remaining budget whole, so their digests report
    // the full framed size; the sixteenth is the last of the round, clipped to the
    // remaining budget (the drain-bounded read caps at the leftover bytes). This test
    // sets no tool-call budget, so the round carries no budget notice and nothing reserves
    // notice bytes: the clipped live result is exactly the leftover, and the digest reports
    // exactly that size.
    let expected_clipped = turn_budget - 15 * framed_len;
    assert!(
        expected_clipped < framed_len,
        "the fixture must leave a clipped tail: remainder {expected_clipped} framed {framed_len}"
    );
    for call in &retained_calls[..15] {
        assert!(
            call.result.contains(&format!("bytes={framed_len}")),
            "the first fifteen reads keep their full framed size: {}",
            call.result
        );
    }
    let last = retained_calls[15].result.clone();
    assert!(
        last.contains(&format!("bytes={expected_clipped}")),
        "the last read is clipped to the remaining budget: {last}"
    );
    assert_ne!(
        last, retained_calls[0].result,
        "the clipped tail renders a different digest record than a whole read"
    );

    // The charged live bytes are exactly the sum the digest records report, and they
    // never exceed the shared output budget.
    let charged: usize = retained_calls
        .iter()
        .map(|call| {
            let header = call.result.lines().next().unwrap_or("");
            header
                .split("bytes=")
                .nth(1)
                .and_then(|rest| rest.split(' ').next())
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or_else(|| panic!("digest record reports bytes=: {call:?}"))
        })
        .sum();
    assert_eq!(
        charged,
        15 * framed_len + expected_clipped,
        "the charged live bytes are exactly the sizes the records report"
    );
    assert!(
        charged <= turn_budget,
        "the live aggregate stays inside the shared output budget: {charged}"
    );
    let output_bytes: usize = retained_calls.iter().map(|call| call.result.len()).sum();
    assert!(
        output_bytes < 16 * 1024,
        "16 bounded digest records are retained, not 16 MiB of raw output: {output_bytes}"
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
}

/// The end-to-end #66 path the unit test `tool_call_record_durable_form_omits_result_live`
/// (src/session/tests.rs) summarizes: a bulk read's admitted sanitized bytes ride the
/// NEXT provider request as the live projection, while the session's durable transcript/state
/// records carry only the one bounded CTXDIGEST digest and never the raw fixture bytes.
///
/// The capturing backend records every model request the agent actually sends. After the run the
/// second call's final request (the tool-return message) must carry the framed read window
/// `[0..N of N bytes]\n` plus both fixture sentinels, and no `CTXDIGEST` anywhere in
/// it. A re-open through the authenticated `SessionStore::load` (never a hand-parse)
/// then shows the persisted call as one CTXDIGEST v1 record whose `bytes=` matches the
/// fingerprint of the very string the request carried, whose `content-…` handle is the
/// production basis (`context_kernel::canonical::digest`) over those same admitted bytes,
/// and whose `result_live` reloads empty (`#[serde(default, skip_serializing)]`). Only
/// after that authenticated reopen succeeds do we scan the durable session-dir records for the
/// `result_live` key and the raw fixture bytes, skipping the `context/` artifacts where the
/// admitted bytes legitimately live.
#[test]
fn admitted_live_bytes_are_digest_only_after_authenticated_reopen() {
    let cwd = new_cwd();
    let interior_sentinel = "interior_sentinel_9f3c7";
    let final_sentinel = "final_sentinel_c84d2";
    let body_line = "fn helper() -> u32 { 41 + 1 }\n";
    let mut fixture = String::from(
        "// digest-only live-bytes fixture\n// module: admitted_live_bytes_are_digest_only\n",
    );
    fixture.push_str(body_line);
    fixture.push_str(interior_sentinel);
    fixture.push('\n');
    // Pad a modest ordinary source file past the bulk threshold (BULK_RESULT_BYTES = 1024)
    // while staying far under the 1 MiB read bound, so the whole file is one read window.
    while fixture.len() < 2048 {
        fixture.push_str(body_line);
    }
    fixture.push_str(final_sentinel);
    fixture.push('\n');
    let file_len = fixture.len();
    assert!(
        file_len >= 1024,
        "the fixture must cross the bulk threshold: {file_len}"
    );
    // read_file frames the whole window as "[0..N of N bytes]\n" before the content, so
    // the admitted payload is the fixture plus that header; the digest reports the framed size.
    let frame_header = format!("[0..{file_len} of {file_len} bytes]\n");
    let framed_len = file_len + frame_header.len();
    std::fs::write(cwd.join("digest-live-fixture.rs"), &fixture).unwrap();

    let st = store("sauthreopen");
    let r = reserved(&st, None, None, "P1", &cwd).unwrap();
    let round = LlmResult {
        usage: LlmUsage::default(),
        text: "reading".to_string(),
        calls: vec![ToolCall {
            id: "live-read".into(),
            name: "read_file".into(),
            args_json: r#"{"path":"digest-live-fixture.rs"}"#.into(),
        }],
        finish_reason: Some(FinishReason::ToolCall),
    };
    let backend = CapturingBackend::new(vec![round, result("done")]);
    let captured = backend.captured_handle();
    let a = agent(Box::new(backend), &cwd);
    a.run(&st, &r).expect("the bounded turn succeeds");
    drop(a);

    // The first call carries the opening request list; the second carries the round's
    // assistant message followed by the tool-return message with the admitted live bytes.
    let batches = captured.lock().unwrap();
    assert_eq!(batches.len(), 2, "one opening call and one post-round call");
    let second = batches
        .last()
        .expect("the second backend call was captured");
    assert_eq!(second.len(), 4, "system, prompt, assistant, tool-return");
    let tool_return = second
        .last()
        .expect("the tool-return message is the final request");
    let live = tool_return
        .parts
        .iter()
        .find_map(|part| match part {
            serdes_ai::core::ModelRequestPart::ToolReturn(tr) => {
                Some(tr.content.to_string_content())
            }
            _ => None,
        })
        .expect("the final request carries a tool-return part");
    assert_eq!(
        live,
        format!("{frame_header}{fixture}"),
        "the admitted live bytes are exactly the framed read window"
    );
    assert!(
        live.starts_with(&frame_header),
        "the tool-return carries the read_file window framing"
    );
    assert!(
        live.contains(interior_sentinel),
        "the interior sentinel rides the live projection"
    );
    assert!(
        live.contains(final_sentinel),
        "the final sentinel rides the live projection"
    );
    let serialized = serde_json::to_string(tool_return).unwrap();
    assert!(
        !serialized.contains("CTXDIGEST"),
        "the request list carries live bytes, never the digest record"
    );
    assert_eq!(
        live.len(),
        framed_len,
        "bytes= counts the framed admitted payload"
    );
    drop(batches);

    // The durable side is the ONE bounded CTXDIGEST record. Reopen ONLY through the
    // authenticated loader (the digest-chained session log and state slots are validated
    // there; we never hand-parse unauthenticated JSON as authentication evidence).
    drop(st);
    let sid = SessionId::parse("sauthreopen").unwrap();
    let st2 = SessionStore::load(&sid).expect("authenticated reopen succeeds");
    let snap = st2.snapshot().unwrap();
    let branch = snap
        .branches
        .iter()
        .find(|b| b.branch_id == r.branch_id)
        .unwrap();
    assert_eq!(branch.lifecycle, Lifecycle::Completed);
    let call = &branch.rounds[0].calls[0];
    assert!(
        call.result.starts_with("CTXDIGEST v1 tool=read_file "),
        "the bulk result is retained as a digest record: {}",
        call.result
    );
    assert!(
        call.result.contains(&format!("bytes={framed_len}")),
        "the header reports the framed admitted payload size: {}",
        call.result
    );
    let handle = digest_handle(&call.result);
    assert!(
        handle.starts_with("content-") && handle.len() == "content-".len() + 16,
        "the record carries a content digest handle, not a vault slot: {handle}"
    );
    // Tie the header's handle to the captured live bytes through the PRODUCTION basis
    // (context_kernel::canonical::digest, the same content_digest_handle source),
    // never a second invented digest.
    let production = format!(
        "content-{:016x}",
        llxprt_code_rs::context_kernel::canonical::digest(live.as_bytes())
    );
    assert_eq!(
        handle, production,
        "the authenticated record's handle is the production content digest of the admitted bytes"
    );
    assert_eq!(
        call.result_live, "",
        "result_live is transient and reloads empty (skip_serializing)"
    );

    // Only after the authenticated reopen succeeds: the durable transcript/state records
    // (NOT the context/ artifacts, which legitimately retain the admitted bytes) must never
    // carry the result_live key nor the raw fixture bytes.
    let mut durable = Vec::new();
    for entry in std::fs::read_dir(&st2.session_dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if !entry.file_type().unwrap().is_file() {
            continue;
        }
        durable.extend(std::fs::read(&path).unwrap());
    }
    let durable_text = String::from_utf8_lossy(&durable);
    assert!(
        !durable_text.contains("result_live"),
        "the durable state never serializes the transient live projection"
    );
    assert!(
        !durable_text.contains(interior_sentinel),
        "the raw fixture interior never reaches the durable transcript"
    );
    assert!(
        !durable_text.contains(final_sentinel),
        "the raw fixture tail never reaches the durable transcript"
    );
}

/// The exhaustion boundary of the shared output budget: once the LIVE bytes charged so
/// far reach MAX_TURN_OUTPUT_BYTES, the next call fails BEFORE it executes with the
/// output-cap refusal (AgentError key "limit"). The steps are sized so the budget is
/// exhausted exactly, so the boundary itself - not an overshoot - is what refuses.
///
/// The refusal persists only the prior COMPLETED rounds of the turn; the one round
/// carrying the failing call is dropped together with its already-executed records (the
/// `dead` path is a pre-checkpoint failure, so its local round never commits). Dropping
/// the records does NOT undo the executed filesystem writes: a checkpoint failure is not
/// a rollback, so the granted write's file survives on disk with exactly the bytes the
/// tool wrote, while the refused call never executed and its file stays absent. The
/// positive control below replays the same granted prefix through the tail read, so it
/// verifies the same charged-byte sum the refusal asserts - the whole cap - while
/// committing its round and its own written file. The agent is never asked for a next
/// model reply, because the refusal fires mid-round.
#[test]
fn output_budget_exhaustion_fails_the_next_call_before_execution() {
    use llxprt_code_rs::agent::OutputCaps;

    let cwd = new_cwd();
    // read_file frames its window as "[0..N of N bytes]\n" ahead of the content, so the
    // released bytes are the payload plus that header; the digest reports the framed size.
    let payload_len = 1024;
    let frame_header = format!("[0..{payload_len} of {payload_len} bytes]\n");
    let framed_len = payload_len + frame_header.len();
    // Keep the exact exhaustion proof small: the production threshold remains 1024
    // bytes, while this test's explicitly resolved turn cap is sixteen framed reads.
    let turn_budget = 16 * framed_len;
    std::fs::write(cwd.join("megabyte.txt"), "a".repeat(payload_len)).unwrap();
    // One granted write that MUST execute, and then the one that must NOT. Nothing is
    // pre-created: both write targets are absent before the turn, so the files below are
    // written by the tool calls themselves (or not at all). The grant of that one write and
    // the one tail read below are derived so the three steps land exactly on the boundary:
    // fifteen whole framed reads, plus that exact grant, plus a file whose clipped framed
    // read fills precisely the leftover, leave the FINAL call at remaining == 0.
    let granted_text = "nine bytes";
    let granted_bytes = granted_text.as_bytes();
    let granted_path = "granted.txt";
    // The write tool's result is exactly "wrote {content-len} bytes to {path}" (src/tools.rs
    // `write_file_tool`), where the path is the leaf-name `PathBuf` `atomic_write` returns,
    // so the grant is derived from that spelling - 10 content bytes render "wrote 10 bytes
    // to granted.txt" - rather than from a hand-counted length.
    let granted = format!(
        "wrote {} bytes to {}",
        granted_bytes.len(),
        Path::new(granted_path).display()
    );
    assert_eq!(
        granted_bytes.len(),
        10,
        "the fixture's written payload size"
    );
    let budget_after_fifteen = turn_budget - 15 * framed_len;
    // The grant charges exactly the leftover minus the tail read's clipped size; the tail
    // file is written so its framed read - capped to the remaining budget - charges the
    // last byte.
    let budget_for_tail = budget_after_fifteen - granted.len();
    std::fs::write(
        cwd.join("tail.txt"),
        "t".repeat(budget_for_tail.saturating_sub(1)),
    )
    .unwrap();
    assert_eq!(
        granted.len() + budget_for_tail,
        budget_after_fifteen,
        "the fixture must leave exactly zero live budget for the final call"
    );
    assert!(
        !cwd.join(granted_path).exists() && !cwd.join("refused-evidence.txt").exists(),
        "both write targets start absent, so any file below is this turn's own doing"
    );

    let st = store("sagg-output-exhausted");
    let request = reserved(&st, None, None, "P1", &cwd).unwrap();
    let reads = (0..15).map(|index| ToolCall {
        id: format!("read-{index}"),
        name: "read_file".into(),
        args_json: r#"{"path":"megabyte.txt"}"#.into(),
    });
    let write_ok = ToolCall {
        id: "write-evidenced".into(),
        name: "write_file".into(),
        args_json: format!(r#"{{"path":"{granted_path}","content":"{granted_text}"}}"#),
    };
    let tail = ToolCall {
        id: "read-tail".into(),
        name: "read_file".into(),
        args_json: r#"{"path":"tail.txt"}"#.into(),
    };
    // This last call must be refused BEFORE execution: its file must therefore never appear.
    let refused = ToolCall {
        id: "refused-evidence".into(),
        name: "write_file".into(),
        args_json: r#"{"path":"refused-evidence.txt","content":"must not run"}"#.into(),
    };
    let calls: Vec<ToolCall> = reads
        .clone()
        .chain([write_ok.clone(), tail.clone(), refused])
        .collect();
    let tool_round = LlmResult {
        usage: LlmUsage::default(),
        text: "reading".into(),
        calls,
        finish_reason: Some(FinishReason::ToolCall),
    };
    // The turn budget is the subject here, not the context limit: the FIFTEEN frames that
    // ride the one post-round request are ~15.7 MiB of LIVE bytes, so this sets a context
    // large enough to hold them (the context is counted in tokens at the 3-bytes-per-token
    // heuristic guard). A smaller expression would trip the pre-send context gate BEFORE the
    // shared output budget ever binds, exactly as the completed sibling test documents.
    // (Bound to `refusing` so the positive control below can still reach the `agent`
    // helper instead of this value.)
    let refusing = agent(
        Box::new(MockBackend::new(vec![tool_round, result("done")])),
        &cwd,
    )
    .with_context_limit(Some(turn_budget as u64))
    .with_output_caps(OutputCaps {
        turn: turn_budget,
        ..OutputCaps::default()
    });

    let error = refusing
        .run(&st, &request)
        .expect_err("the exhausted budget must refuse the next call");
    assert_eq!(error.key, "limit", "{error:?}");
    assert!(
        error
            .message
            .contains(&format!("reached the {turn_budget} byte cap")),
        "the refusal names the output cap: {error:?}"
    );
    // Direct execution evidence: the granted write RAN, and the refused write did NOT. The
    // grant executed before the budget ran out, and a session checkpoint failure never rolls
    // back an already-executed filesystem write: granted.txt is on disk with exactly the
    // bytes the tool wrote. The refused call never executed, so its file is absent. The
    // refusal fires on the turn's first local round, so its records never complete/commit
    // and the model made exactly one call.
    assert_eq!(
        refusing.model_calls(),
        1,
        "the refusal happens before the next model request"
    );
    assert_eq!(
        std::fs::read(cwd.join(granted_path)).unwrap_or_default(),
        granted_bytes,
        "the executed write survived the refusal: its file carries exactly the tool-written bytes"
    );
    assert!(
        !cwd.join("refused-evidence.txt").exists(),
        "the refused call never executed: the side-effecting write left no file"
    );

    // The failed local round never committed, so the persisted branch is terminal and EMPTY: the
    // `dead` refusal carries the COMPLETED-round list, which is none here, because the
    // exhaustion happened inside the round that was still executing. The on-disk
    // side-effect assertions above - not a round count - are what prove what ran.
    let snapshot = st.snapshot().unwrap();
    let branch = snapshot
        .branches
        .iter()
        .find(|branch| branch.branch_id == request.branch_id)
        .unwrap();
    assert_eq!(branch.lifecycle, Lifecycle::Failed);
    assert!(
        branch.rounds.is_empty(),
        "no completed round is persisted: the refusing round is the turn's first and failed local"
    );

    // Independent positive pass over the SAME live-byte derivation: a run that replays the
    // granted prefix THROUGH the tail read - only the final refused write short of the
    // boundary - charges the whole cap and completes, proving the derivation above is
    // exactly where the refusal must (and does) land. It runs in its own freshly seeded
    // workspace whose granted.txt starts absent, so its committed file is that run's own
    // executed-write proof rather than an inherited artifact of the refused run.
    let ok_st = store("sagg-output-exhausted-ok");
    let ok_cwd = new_cwd();
    std::fs::write(ok_cwd.join("megabyte.txt"), "a".repeat(payload_len)).unwrap();
    std::fs::write(
        ok_cwd.join("tail.txt"),
        "t".repeat(budget_for_tail.saturating_sub(1)),
    )
    .unwrap();
    assert!(
        !ok_cwd.join(granted_path).exists(),
        "the positive control's write target starts absent"
    );
    let ok_reserved = reserved(&ok_st, None, None, "P1", &ok_cwd).unwrap();
    let below: Vec<ToolCall> = reads.chain([write_ok, tail]).collect();
    let below_round = LlmResult {
        usage: LlmUsage::default(),
        text: "reading".into(),
        calls: below,
        finish_reason: Some(FinishReason::ToolCall),
    };
    let ok_agent = agent(
        Box::new(MockBackend::new(vec![below_round, result("done")])),
        &ok_cwd,
    )
    .with_context_limit(Some(turn_budget as u64))
    .with_output_caps(OutputCaps {
        turn: turn_budget,
        ..OutputCaps::default()
    });
    ok_agent
        .run(&ok_st, &ok_reserved)
        .expect("under budget succeeds");
    let ok_snapshot = ok_st.snapshot().unwrap();
    let ok_branch = ok_snapshot
        .branches
        .iter()
        .find(|branch| branch.branch_id == ok_reserved.branch_id)
        .unwrap();
    assert_eq!(ok_branch.lifecycle, Lifecycle::Completed);
    assert_eq!(
        ok_branch.rounds.len(),
        2,
        "the tool round and the final assistant round both persist"
    );
    assert_eq!(ok_branch.rounds[0].calls.len(), 17);
    assert_eq!(ok_branch.rounds[1].assistant, "done");
    assert!(
        ok_branch.rounds[0].calls.iter().all(|call| !call.refused),
        "an under-budget round never refuses a call"
    );
    // The retained records state the charged bytes themselves: the fifteen whole framed
    // reads, the granted write's own result string, and the tail's clipped read, summing
    // to the whole shared cap with nothing refused.
    let witnessed = &ok_branch.rounds[0].calls;
    for call in &witnessed[..15] {
        assert_eq!(
            call.result_live.len(),
            framed_len,
            "each whole framed read charges its full framed size: {:?}",
            call.result_live
        );
    }
    assert_eq!(
        witnessed[15].result_live, granted,
        "the granted write's result is carried verbatim, not digested"
    );
    assert_eq!(
        witnessed[15].result_live.len(),
        granted.len(),
        "the granted write charges exactly the result the tool formats"
    );
    assert!(
        witnessed[16].result_live.len() < framed_len,
        "the tail read is clipped to what the cap had left: {:?}",
        witnessed[16].result_live
    );
    assert_eq!(
        std::fs::read(ok_cwd.join(granted_path))
            .expect("the committed round keeps the executed write"),
        granted_bytes,
        "the write's file carries exactly the bytes the call wrote"
    );
    let charged: usize = witnessed.iter().map(|call| call.result_live.len()).sum();
    assert_eq!(
        charged, turn_budget,
        "the granted prefix through the tail charges the whole shared cap"
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
        usage: LlmUsage::default(),
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
        112,
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
    use llxprt_code_rs::limits::TIMEOUT_LEASE_SECONDS as LEASE_SECONDS;
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
            result_live: "checkpoint result".into(),
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
        usage: LlmUsage::default(),
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
