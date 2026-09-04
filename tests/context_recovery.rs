//! Phase 2 restart and durability tests for the durable context directory.
//!
//! These tests drive the public session/agent seams only (no internal APIs), so
//! they exercise exactly what a later process would re-read after a crash: the
//! context vault key, the reloaded sanitized spine, the restored vault slots,
//! the reloaded filter version histories, and the refusal to complete a branch
//! whose context artifacts never landed.

use llxprt_code_rs::adapter::{ChatBackend, LlmResult, ToolCall};
use llxprt_code_rs::agent::CodingAgent;
use llxprt_code_rs::session::{Lifecycle, ReservedRequest, SessionId, SessionStore, StoreError};
use llxprt_code_rs::tools::ToolSpec;
use serdes_ai::core::FinishReason;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// A scripted backend that never contacts a network: each call pops the next
/// canned reply, repeating the last when exhausted.
struct MockBackend {
    replies: Mutex<std::collections::VecDeque<LlmResult>>,
    calls: Mutex<usize>,
}

fn result(text: &str) -> LlmResult {
    LlmResult {
        text: text.to_string(),
        calls: Vec::new(),
        finish_reason: Some(FinishReason::Stop),
    }
}

impl ChatBackend for MockBackend {
    fn request(
        &self,
        _requests: &[serdes_ai::core::ModelRequest],
        _tools: &[ToolSpec],
    ) -> Result<LlmResult, String> {
        *self.calls.lock().unwrap() += 1;
        let mut queue = self.replies.lock().unwrap();
        Ok(queue.pop_front().unwrap_or_else(|| result("fallback")))
    }

    fn request_calls(&self) -> usize {
        *self.calls.lock().unwrap()
    }
}

impl MockBackend {
    fn new(replies: Vec<LlmResult>) -> Self {
        MockBackend {
            replies: Mutex::new(replies.into()),
            calls: Mutex::new(0),
        }
    }
}

static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Per-test-binary configuration root; every test opens its store inside it.
fn root() -> PathBuf {
    ROOT.get_or_init(|| {
        let root = std::env::temp_dir().join(format!("llxprt-rs-ctxrec-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        root
    })
    .clone()
}

/// A fresh workspace directory for one test.
fn workspace() -> PathBuf {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = root().join(format!("ws-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Opens a unique session store inside the shared root.
fn store(id: &str) -> SessionStore {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    // Session ids must be unique per store: a shared root means two live
    // stores with the same id contend for the same session lock.
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let sid = SessionId::parse(&format!("{id}-{n}")).unwrap();
    SessionStore::load_at(&sid, &root()).expect("open store")
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

/// Reads one published `context/` artifact of a session.
fn artifact(store: &SessionStore, name: &str) -> Vec<u8> {
    std::fs::read(store.session_dir.join("context").join(name))
        .unwrap_or_else(|error| panic!("read context artifact {name} failed: {error}"))
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

/// Reads the sanitized evidence the reloaded store exposes, paging forward from
/// zero until the spine reports no remaining bytes, so multi-generation
/// assertions can inspect the whole record. The spine length is not published,
/// so each page asks for one bounded window at a time and stops as soon as a
/// window is refused or returns nothing.
fn page_text(store: &SessionStore) -> Vec<u8> {
    const WINDOW: u64 = 64 * 1024;
    let mut out = Vec::new();
    let mut start = 0u64;
    loop {
        let page = match store.context_read_page(start..start + WINDOW, WINDOW as usize) {
            Ok(page) => page,
            Err(reason) if reason.contains("spine") => return out,
            Err(reason) => panic!("read spine: {reason}"),
        };
        let got = page.bytes.len() as u64;
        out.extend_from_slice(&page.bytes);
        if got == 0 {
            return out;
        }
        start += got;
    }
}

/// Runs one attempt that reads a 64 KiB file so a bulk result is ingested, and
/// returns the store for further inspection.
fn run_bulk_turn(store: &SessionStore, cwd: &Path, name: &str, id: &str) {
    std::fs::write(cwd.join(name), "q".repeat(64 * 1024)).unwrap();
    let round = LlmResult {
        text: "reading".into(),
        calls: vec![ToolCall {
            id: id.to_string(),
            name: "read_file".into(),
            args_json: format!(r#"{{"path":"{name}"}}"#),
        }],
        finish_reason: Some(FinishReason::ToolCall),
    };
    let cwd = cwd.to_path_buf();
    let store = reopen(store);
    let reserved = reserved(&store, None, None, "P1", &cwd).unwrap();
    let a = agent(
        Box::new(MockBackend::new(vec![round, result("done")])),
        &cwd,
    );
    let out = a.run(&store, &reserved).expect("bulk turn runs");
    assert_eq!(out.status, "ok", "the attempt completes");
}

/// Reopens the same session under a fresh store handle, as a later process would.
fn reopen(store: &SessionStore) -> SessionStore {
    SessionStore::load_at(
        &SessionId::parse(store.session_id.as_str()).unwrap(),
        &root(),
    )
    .expect("reopen store")
}

/// The vault key is private per-session entropy, not derivable from the session id.
#[test]
fn vault_key_is_stored_privately_and_differs_per_session() {
    let cwd = workspace();
    let a = store("vault-key-a");
    let b = store("vault-key-b");
    let r1 = reserved(&a, None, None, "P1", &cwd).unwrap();
    let r2 = reserved(&b, None, None, "P1", &cwd).unwrap();
    let agent_a = agent(Box::new(MockBackend::new(vec![result("done")])), &cwd);
    let agent_b = agent(Box::new(MockBackend::new(vec![result("done")])), &cwd);
    agent_a.run(&a, &r1).unwrap();
    agent_b.run(&b, &r2).unwrap();
    let key_a = std::fs::read(a.session_dir.join("context-vault-key")).unwrap();
    let key_b = std::fs::read(b.session_dir.join("context-vault-key")).unwrap();
    assert_eq!(key_a.len(), 32, "the vault key is 32 bytes of key material");
    assert_ne!(key_a, key_b, "two sessions never share one vault key");
    // The seed used to derive keys on main is public and digest-independent of
    // the session id, so the stored key must not be a deterministic function of
    // the session id alone.
    let mode = std::fs::metadata(a.session_dir.join("context-vault-key"))
        .unwrap()
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(mode.mode() & 0o777, 0o600, "the vault key is 0600");
    }
}

/// After a restart, a reloaded store resolves the same evidence: the spine is
/// re-framed under content-stable handles, the restored vault slot reads back,
/// and the historical filter versions reload.
#[test]
fn restart_reopens_spine_vault_and_filter_versions() {
    let cwd = workspace();
    let first = store("restart-reopen");
    run_bulk_turn(&first, &cwd, "bulk.txt", "c0");

    let snapshot = first.snapshot().unwrap();
    let branch = snapshot
        .branches
        .iter()
        .find(|b| b.branch_id == "b1")
        .expect("the branch completed");
    assert_eq!(branch.lifecycle, Lifecycle::Completed);
    let record = &branch.rounds[0].calls[0].result;
    assert!(
        record.starts_with("CTXDIGEST v1 tool=read_file "),
        "the bulk result is retained as a digest record: {record}"
    );
    // The record names a content digest handle, never a vault slot.
    let before_handle = digest_handle(record).to_string();
    assert!(
        before_handle.starts_with("content-"),
        "the digest names a content handle, not a vault slot: {before_handle}"
    );

    // A later process reopens the store and ingests new evidence: recovery
    // reopens the durable spine, so the reloaded pages must hold both the
    // previous run's bytes and the new ones.
    let second = reopen(&first);
    std::fs::write(cwd.join("second.txt"), "r".repeat(64 * 1024)).unwrap();
    let round = LlmResult {
        text: "reading".into(),
        calls: vec![ToolCall {
            id: "c1".into(),
            name: "read_file".into(),
            args_json: r#"{"path":"second.txt"}"#.into(),
        }],
        finish_reason: Some(FinishReason::ToolCall),
    };
    let second_turn = reserved(&second, Some(1), None, "P2", &cwd).unwrap();
    let a = agent(
        Box::new(MockBackend::new(vec![round, result("done")])),
        &cwd,
    );
    a.run(&second, &second_turn)
        .expect("the restarted turn runs on the recovered store");
    let text = page_text(&second);
    assert!(
        text.len() >= 128 * 1024,
        "the reloaded spine holds evidence from both processes: {} bytes",
        text.len()
    );
    assert!(
        text.windows(64).any(|w| w == b"q".repeat(64).as_slice())
            && text.windows(64).any(|w| w == b"r".repeat(64).as_slice()),
        "both generations of sanitized evidence are reachable after recovery"
    );
    // The vault artifact is non-empty and restorable, and the manifest reloads
    // the historical rule and vocabulary versions.
    let vault = artifact(&second, "vault");
    assert!(!vault.is_empty(), "the vault snapshot is durable");
    let manifest: serde_json::Value =
        serde_json::from_slice(&artifact(&second, "manifest.json")).unwrap();
    assert!(
        !manifest["rules"].as_array().unwrap().is_empty(),
        "rule version history persists"
    );
    assert!(
        !manifest["vocabularies"].as_array().unwrap().is_empty(),
        "vocabulary version history persists"
    );
    assert_eq!(manifest["mode"], "normal", "the mode reloads");
}

/// A corrupt sanitized spine is an integrity failure, never a silent truncation:
/// the restarted exchange refuses to advance instead of rewriting history.
#[test]
fn corrupt_spine_fails_the_exchange_instead_of_truncating() {
    let cwd = workspace();
    let first = store("corrupt-spine");
    run_bulk_turn(&first, &cwd, "bulk.txt", "c0");

    // Corrupt one byte inside the first frame's payload.
    let spine = first.session_dir.join("context").join("sanitized");
    let mut bytes = std::fs::read(&spine).unwrap();
    assert!(bytes.len() > 64, "the spine is not empty");
    let payload_start = 8 + 8; // length field + digest field
    bytes[payload_start] ^= 0xff;
    std::fs::write(&spine, &bytes).unwrap();

    let second = reopen(&first);
    // A second turn on the same session must fail the exchange rather than
    // silently starting from an empty store: the reservation succeeds (the
    // corruption is in the context directory, not the session log), and the
    // context recovery happens when the run digests its first bulk result.
    let second_turn = reserved(&second, Some(1), None, "P2", &cwd).unwrap();
    let a = agent(Box::new(MockBackend::new(vec![result("done")])), &cwd);
    let refused = a.run(&second, &second_turn);
    assert!(
        refused.is_err(),
        "a corrupt spine must fail the turn instead of rewriting history"
    );
    let snapshot = second.snapshot().unwrap();
    let still_completed = snapshot
        .branches
        .iter()
        .filter(|b| b.lifecycle == Lifecycle::Completed)
        .count();
    assert_eq!(
        still_completed, 1,
        "only the previously completed branch stays completed"
    );
    assert!(
        artifact(&second, "sanitized").len() > 8,
        "the corrupt spine is never silently truncated and rewritten"
    );
}

/// A digest record that would carry an oversized preserved span is elided
/// instead of exceeding the record byte budget.
#[test]
fn digest_records_stay_inside_the_preserved_span_byte_budget() {
    let cwd = workspace();
    let store = store("span-budget");
    // One line far larger than the span byte budget, so the first preserved
    // span alone would overflow the record budget.
    let big = format!("needle {}\n", "w".repeat(8 * 1024));
    std::fs::write(cwd.join("span.txt"), &big).unwrap();
    let round = LlmResult {
        text: "reading".into(),
        calls: vec![ToolCall {
            id: "c0".into(),
            name: "read_file".into(),
            args_json: r#"{"path":"span.txt"}"#.into(),
        }],
        finish_reason: Some(FinishReason::ToolCall),
    };
    let reserved = reserved(&store, None, None, "P1", &cwd).unwrap();
    let agent = agent(
        Box::new(MockBackend::new(vec![round, result("done")])),
        &cwd,
    );
    agent.run(&store, &reserved).expect("bounded turn runs");
    let snapshot = store.snapshot().unwrap();
    let branch = snapshot
        .branches
        .iter()
        .find(|b| b.branch_id == reserved.branch_id)
        .unwrap();
    let record = &branch.rounds[0].calls[0].result;
    assert!(
        record.len() < 2048,
        "the record stays inside the span byte budget: {} bytes",
        record.len()
    );
    assert!(
        !record.contains(&"w".repeat(1024)),
        "no oversized preserved span is carried in the record"
    );
    assert!(
        record.starts_with("CTXDIGEST v1 tool=read_file "),
        "the record is still a digest record: {record}"
    );
}

/// BranchCompleted cannot fire before the context artifacts are durable: an
/// unwritable context directory fails the turn instead of completing the branch.
#[test]
fn branch_completion_requires_durable_context_artifacts() {
    let cwd = workspace();
    let store = store("branch-durability");
    let first_turn = reserved(&store, None, None, "P1", &cwd).unwrap();
    let a = agent(Box::new(MockBackend::new(vec![result("done")])), &cwd);
    a.run(&store, &first_turn).expect("first turn runs");

    // The context directory is a file, so every later publication fails.
    let context = store.session_dir.join("context");
    std::fs::remove_dir_all(&context).unwrap();
    std::fs::write(&context, "blocked").unwrap();

    let before = store.snapshot().unwrap();
    let second = reserved(&store, Some(1), None, "P2", &cwd).unwrap();
    let a = agent(Box::new(MockBackend::new(vec![result("done")])), &cwd);
    let error = a.run(&store, &second).expect_err("the turn must fail");
    assert!(
        !error.to_string().is_empty(),
        "the turn reports the context refusal"
    );
    let after = store.snapshot().unwrap();
    // The second branch is not completed: its rounds were never made durable.
    let completed_before = before
        .branches
        .iter()
        .filter(|b| b.lifecycle == Lifecycle::Completed)
        .count();
    let completed_after = after
        .branches
        .iter()
        .filter(|b| b.lifecycle == Lifecycle::Completed)
        .count();
    assert_eq!(
        completed_after, completed_before,
        "no branch completes whose context artifacts did not land"
    );
}

/// An admission that does not fit the executor's region budget is refused before
/// the transaction appends anything: the spine stays untouched, so the
/// transaction core really is the only path that adds spine bytes.
#[test]
fn oversized_admission_is_refused_without_touching_the_spine() {
    let cwd = workspace();
    let store = store("admission-refused");
    let first_turn = reserved(&store, None, None, "P1", &cwd).unwrap();
    let a = agent(Box::new(MockBackend::new(vec![result("done")])), &cwd);
    a.run(&store, &first_turn).expect("first turn runs");
    let before = artifact(&store, "sanitized").len();

    // A bulk result larger than the whole admission region cannot satisfy the
    // executor's `bound <= B - R - H` precondition, so the admission is
    // refused before any spine byte is written.
    let oversized = 40 << 20;
    let compacted = store.compact_tool_result("read_file", &"h".repeat(oversized));
    assert!(
        compacted.starts_with("CTXDIGEST v1") || compacted.contains("quiesce"),
        "the refusal still yields a bounded record, not raw bytes: {compacted}"
    );
    let after = artifact(&store, "sanitized").len();
    assert_eq!(
        after, before,
        "no spine bytes are appended when the executor refuses the admission"
    );
}

/// A production-path secret corpus never lands unscanned: routing bulk results
/// through the ingress transaction means the redactor's substitutions are the
/// only bytes the sanitized spine and the vault ever see (issue #100: production
/// redaction bypass). The corpus is the same one the component tests use, padded above
/// the bulk threshold so the pre-entry compaction seam exercises it.
#[test]
fn production_secret_corpus_never_reaches_the_durable_artifacts() {
    const SECRET: &str = "CTXEVAL-SECRET-A1B2C3D4E5";
    let cwd = workspace();
    let store = store("prod-secret-corpus");
    let first_turn = reserved(&store, None, None, "P1", &cwd).unwrap();
    let a = agent(Box::new(MockBackend::new(vec![result("done")])), &cwd);
    a.run(&store, &first_turn).expect("first turn runs");

    // The corpus marker is a detector class the redactor replaces in place;
    // padding below the threshold would skip the compaction seam entirely.
    let payload = format!(
        "marker: {SECRET}\nexact error span: bytes 4096..4131 \"unexpected trailing frame\"\n{}",
        "noise line 0000\n".repeat(96)
    );
    assert!(payload.len() > 1024, "the corpus is bulk evidence");
    let compacted = store.compact_tool_result("read_file", &payload);
    assert!(
        compacted.starts_with("CTXDIGEST v1 tool=read_file "),
        "the bulk corpus is digested on the production path: {compacted}"
    );
    assert!(
        !compacted.contains(SECRET),
        "the compact record carries no unscanned secret"
    );

    // Every durable context artifact and the session log stay free of the
    // secret: the spine holds the sanitized bytes only, and the vault holds
    // the quarantined payload sealed, never in the clear.
    let sanitized = artifact(&store, "sanitized");
    assert!(
        !String::from_utf8_lossy(&sanitized).contains(SECRET),
        "the sanitized spine never holds an unscanned secret"
    );
    let vault = artifact(&store, "vault");
    assert!(
        !String::from_utf8_lossy(&vault).contains(SECRET),
        "the vault artifact is ciphertext, never the plaintext secret"
    );
    let events = artifact(&store, "events.log");
    assert!(
        !String::from_utf8_lossy(&events).contains(SECRET),
        "the policy event log never names a secret"
    );
    let journal = artifact(&store, "rewrite-journal.log");
    assert!(
        !String::from_utf8_lossy(&journal).contains(SECRET),
        "the rewrite journal never carries a secret"
    );
    let manifest = artifact(&store, "manifest.json");
    assert!(
        !String::from_utf8_lossy(&manifest).contains(SECRET),
        "the manifest never carries a secret"
    );
    // Both session-state slots are scanned: the durable state is the
    // transcript the next process would replay.
    for name in ["session.json", "session.alt.json"] {
        let path = store.session_dir.join(name);
        if !path.exists() {
            continue;
        }
        let state = std::fs::read_to_string(&path).unwrap();
        assert!(
            !state.contains(SECRET),
            "{name} never carries an unscanned secret"
        );
    }
    let quiesce = store.session_dir.join("context-quiesce.json");
    if quiesce.exists() {
        let marker = std::fs::read_to_string(&quiesce).unwrap();
        assert!(
            !marker.contains(SECRET),
            "the quiesce marker never carries a secret"
        );
    }
    // The sanitized evidence the redactor produced is still reachable, so the
    // secret was replaced in place rather than dropped.
    assert!(
        String::from_utf8_lossy(&sanitized).contains("unexpected trailing frame"),
        "the preserved exact span is still addressable after redaction"
    );
}
