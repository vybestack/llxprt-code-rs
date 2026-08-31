//! Two-`SessionStore` reservation regressions: a live `pending` child at a later
//! turn is found relative to the selected predecessor lineage and the same prompt/turn,
//! so a second store on the same session (a second process) sees the live pending
//! branch and gets `Busy` instead of reserving a duplicate branch with duplicate tool
//! side effects. Completed children replay; failed children retry; a changed prompt
//! forks a new branch. Every assertion is on resulting state and branch ids — never
//! wall-clock.
//!
//! The two stores share the same on-disk session (two independent `SessionStore`
//! values under one config home). All checks are structural.

use llxprt_code_rs::adapter::{ChatBackend, LlmResult};
use llxprt_code_rs::session::{
    BranchRecord, Lifecycle, ReservedRequest, RoundRecord, SessionId, SessionState, SessionStore,
    StoreError,
};
use llxprt_code_rs::tools::ToolSpec;
use serdes_ai::core::FinishReason;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// A backend that records how many calls it made; never used for replay paths.
struct CountingBackend {
    calls: Mutex<usize>,
}

impl ChatBackend for CountingBackend {
    fn request(
        &self,
        _requests: &[serdes_ai::core::ModelRequest],
        _tools: &[ToolSpec],
    ) -> Result<LlmResult, String> {
        *self.calls.lock().unwrap() += 1;
        Ok(LlmResult {
            text: "done".to_string(),
            calls: Vec::new(),
            finish_reason: Some(FinishReason::Stop),
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

/// A per-process config home shared by every store in this test binary. Each
/// integration test binary runs in its own process, so no other binary mutates this.
fn shared_root() -> PathBuf {
    SHARED_ROOT
        .get_or_init(|| {
            let base = std::env::temp_dir().join(format!("llxprt-rs-resv-{}", std::process::id()));
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
    let root = shared_root();
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let w = root.join(format!(
        "ws-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&w).unwrap();
    w
}

fn workspace_identity(path: &Path) -> (u64, u64) {
    llxprt_code_rs::tools::WorkspaceCap::open(path)
        .unwrap()
        .identity()
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

/// Complete turn 1 on `st`.
fn complete_turn1(st: &SessionStore, prompt: &str, cwd: &Path) -> ReservedRequest {
    let r = st.start_request(None, None, prompt, cwd).unwrap();
    st.finalize(
        &r,
        "t1",
        &[RoundRecord {
            assistant: "t1".into(),
            calls: Vec::new(),
        }],
    )
    .unwrap();
    r
}

/// Build a completed branch record for a hand-crafted corrupt/chain state.
fn branch(turn: u32, attempt: u32, id: &str, prompt: &str) -> BranchRecord {
    BranchRecord {
        branch_id: id.to_string(),
        turn,
        attempt,
        parent_branch: None,
        parent_turn: 0,
        parent_attempt: 0,
        prompt: prompt.to_string(),
        digest: llxprt_code_rs::agent::prompt_digest(prompt),
        lifecycle: Lifecycle::Completed,
        rounds: vec![RoundRecord {
            assistant: "done".into(),
            calls: Vec::new(),
        }],
        summary: "done".into(),
        error: String::new(),
        owner: String::new(),
        reserved_at: 0,
        lease_expiry: 0,
    }
}

fn expire_pending(store: &SessionStore) {
    let mut state = store.snapshot().unwrap();
    for branch in &mut state.branches {
        if branch.lifecycle == Lifecycle::Pending {
            branch.lease_expiry = now_secs().saturating_sub(1);
            branch.reserved_at = branch.lease_expiry.saturating_sub(1);
        }
    }
    store.replace_snapshot(&state).unwrap();
}

fn set_lease_expiry(store: &SessionStore, branch_id: &str, expiry: u64) {
    let mut state = store.snapshot().unwrap();
    let branch = state
        .branches
        .iter_mut()
        .find(|branch| branch.branch_id == branch_id)
        .expect("branch exists");
    branch.lease_expiry = expiry;
    if branch.reserved_at >= expiry {
        branch.reserved_at = expiry.saturating_sub(1);
    }
    store.replace_snapshot(&state).unwrap();
}

fn noop_rounds() -> Vec<RoundRecord> {
    vec![RoundRecord {
        assistant: "rejected".into(),
        calls: Vec::new(),
    }]
}

/// Current wall-clock unix seconds (the same basis the store uses).
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Leak a reservation so both the store-scoped borrow in `set_lease_expiry` and the
/// later commit call can use it.
fn leak_reserved(r: ReservedRequest) -> &'static ReservedRequest {
    Box::leak(Box::new(r))
}

/// Complete turn 1, reserve turn 2, and a second store requesting the same
/// turn/prompt while the lease is live must get `Busy` and leave the branch count
/// unchanged (no duplicate reservation, no duplicate tool side effects).
#[test]
fn second_store_gets_busy_for_live_pending_child_branch_count_unchanged() {
    let cwd = new_cwd();
    let st1 = store("resv-busy");
    let st2 = store("resv-busy");

    complete_turn1(&st1, "P1", &cwd);
    let r2 = reserved(&st1, Some(2), None, "P2", &cwd).unwrap();
    assert!(!r2.replay);

    // A second process on the same pending turn/prompt is Busy while the lease is live.
    match reserved(&st2, Some(2), None, "P2", &cwd) {
        Err(StoreError::Busy(_)) => {}
        other => panic!("expected Busy for the live pending child, got {other:?}"),
    }
    let snap = st2.snapshot().unwrap();
    assert_eq!(snap.branches.len(), 2, "Busy must not add a branch");
    // The pending branch is the same one the first store reserved.
    assert_eq!(snap.branches[1].branch_id, r2.branch_id);
    assert_eq!(snap.branches[1].lifecycle, Lifecycle::Pending);
}

/// A second store reclaims a stale pending child **in place** (same branch id), not a
/// new branch, so reclaimed retries never duplicate prior tool side effects.
#[test]
fn second_store_reclaims_stale_pending_child_on_the_same_branch() {
    let cwd = new_cwd();
    let st1 = store("resv-stale");
    let st2 = store("resv-stale");
    complete_turn1(&st1, "P1", &cwd);
    let r2 = reserved(&st1, Some(2), None, "P2", &cwd).unwrap();

    expire_pending(&st1);

    let reclaim = reserved(&st2, Some(2), None, "P2", &cwd).unwrap();
    assert!(
        !reclaim.replay,
        "stale pending must be resumed, not replayed"
    );
    assert!(!reclaim.retry);
    assert_eq!(
        reclaim.branch_id, r2.branch_id,
        "a stale pending child is reclaimed on the same branch"
    );
    assert_ne!(
        reclaim.owner, r2.owner,
        "the second store now owns the lease"
    );
    let snap = st2.snapshot().unwrap();
    assert_eq!(
        snap.branches.len(),
        2,
        "stale reclaim must not add a branch"
    );
}

/// Terminal branches release their live reservation by zeroing lease fields while
/// retaining the owner identity needed for safe idempotent retries.
#[test]
fn terminal_branches_release_their_reservation() {
    let cwd = new_cwd();
    let st = store("resv-release");
    complete_turn1(&st, "P1", &cwd);
    let r2 = reserved(&st, Some(2), None, "P2", &cwd).unwrap();
    st.fail(&r2, "boom", &[]).unwrap();
    let snap = st.snapshot().unwrap();
    assert_eq!(snap.branches.len(), 2);
    let (completed, failed) = (&snap.branches[0], &snap.branches[1]);
    assert_eq!(completed.lifecycle, Lifecycle::Completed);
    assert_eq!(completed.reserved_at, 0);
    assert_eq!(completed.lease_expiry, 0);
    assert_eq!(failed.lifecycle, Lifecycle::Failed);
    assert_eq!(failed.reserved_at, 0);
    assert_eq!(failed.lease_expiry, 0);
}

/// A completed child at the same turn/prompt replays: no new branch, no backend call.
#[test]
fn second_store_replays_completed_child_with_no_model_calls() {
    let cwd = new_cwd();
    let st1 = store("resv-replay");
    let st2 = store("resv-replay");
    complete_turn1(&st1, "P1", &cwd);
    let r2 = reserved(&st1, Some(2), None, "P2", &cwd).unwrap();
    st1.finalize(
        &r2,
        "t2",
        &[RoundRecord {
            assistant: "t2".into(),
            calls: Vec::new(),
        }],
    )
    .unwrap();

    let replay = reserved(&st2, Some(2), None, "P2", &cwd).unwrap();
    assert!(
        replay.replay,
        "a completed child with the same prompt replays"
    );
    assert_eq!(replay.branch_id, r2.branch_id);
    let backend = CountingBackend {
        calls: Mutex::new(0),
    };
    let a = llxprt_code_rs::agent::CodingAgent::with_backend(Box::new(backend), cwd.clone(), false);
    let out = a.run(&st2, &replay).unwrap();
    assert_eq!(out.status, "ok");
    assert!(out.replayed);
    assert_eq!(a.model_calls(), 0, "a replay must never call the backend");
    let snap = st2.snapshot().unwrap();
    assert_eq!(snap.branches.len(), 2);
}

/// A failed child at the same turn/prompt retries into a **new** branch parented to
/// the predecessor turn, never a replay.
#[test]
fn second_store_failed_child_retries_into_a_new_branch() {
    let cwd = new_cwd();
    let st1 = store("resv-retry");
    let st2 = store("resv-retry");
    complete_turn1(&st1, "P1", &cwd);
    let r2 = reserved(&st1, Some(2), None, "P2", &cwd).unwrap();
    st1.fail(&r2, "boom", &[]).unwrap();

    let retry = reserved(&st2, Some(2), None, "P2", &cwd).unwrap();
    assert!(
        retry.retry,
        "a failed prompt becomes a retry, never a replay"
    );
    assert!(!retry.replay);
    assert_ne!(retry.branch_id, r2.branch_id, "a retry is a fresh branch");
    let snap = st2.snapshot().unwrap();
    assert_eq!(snap.branches.len(), 3);
    let rb = &snap.branches[2];
    assert_eq!(rb.lifecycle, Lifecycle::Pending);
    assert_eq!(
        rb.parent_branch.as_deref(),
        Some("b1"),
        "retry parents to turn 1"
    );
    assert_eq!(rb.turn, 2);
}

/// A reservation whose lease is **already expired** (past) can never be finalized or
/// failed: both commit paths must reject with `StoreError::Stale` before writing
/// anything, so rounds/lifecycle/owner/error/summary all stay as reserved.
#[test]
fn finalize_and_fail_reject_expired_lease_leaving_state_unchanged() {
    let cwd = new_cwd();
    let st = store("exp-finalize1");
    let rf = reserved(&st, Some(1), None, "P", &cwd).unwrap();
    set_lease_expiry(&st, &rf.branch_id, now_secs().saturating_sub(10));
    let rf = leak_reserved(rf);
    let rounds = noop_rounds();

    match st.finalize(rf, "unwritten-summary", &rounds) {
        Err(StoreError::Stale) => {}
        other => panic!("finalize on an expired lease must be Stale, got {other:?}"),
    }
    let snap = st.snapshot().unwrap();
    assert_eq!(snap.branches.len(), 1, "finalize: no branch added");
    assert!(
        snap.branches[0].rounds.is_empty(),
        "finalize: rounds unchanged"
    );
    assert_eq!(
        snap.branches[0].lifecycle,
        Lifecycle::Pending,
        "finalize: lifecycle unchanged"
    );
    assert_eq!(
        snap.branches[0].owner, rf.owner,
        "finalize: owner unchanged"
    );
    assert!(
        snap.branches[0].error.is_empty(),
        "finalize: error unchanged"
    );
    assert!(
        snap.branches[0].summary.is_empty(),
        "finalize: summary unchanged"
    );

    let st = store("exp-fail1");
    let rb = reserved(&st, Some(1), None, "P", &cwd).unwrap();
    set_lease_expiry(&st, &rb.branch_id, now_secs().saturating_sub(10));
    let rb = leak_reserved(rb);
    let rounds = noop_rounds();
    match st.fail(rb, "unwritten-error", &rounds) {
        Err(StoreError::Stale) => {}
        other => panic!("fail on an expired lease must be Stale, got {other:?}"),
    }
    let snap = st.snapshot().unwrap();
    assert_eq!(snap.branches.len(), 1, "fail: no branch added");
    assert!(snap.branches[0].rounds.is_empty(), "fail: rounds unchanged");
    assert_eq!(
        snap.branches[0].lifecycle,
        Lifecycle::Pending,
        "fail: lifecycle unchanged"
    );
    assert_eq!(snap.branches[0].owner, rb.owner, "fail: owner unchanged");
    assert!(snap.branches[0].error.is_empty(), "fail: error unchanged");
    assert!(
        snap.branches[0].summary.is_empty(),
        "fail: summary unchanged"
    );
}

/// A reservation whose lease **equals** now is already stale too: `lease_expiry <=
/// now` includes exact equality, so both `finalize` and `fail` must reject with
/// `StoreError::Stale` and leave rounds/lifecycle/owner/error/summary exactly as
/// they were before the attempt.
#[test]
fn finalize_and_fail_reject_exact_now_lease_leaving_state_unchanged() {
    let cwd = new_cwd();
    let st = store("eq-finalize1");
    let rf = reserved(&st, Some(1), None, "P", &cwd).unwrap();
    set_lease_expiry(&st, &rf.branch_id, now_secs());
    let rf = leak_reserved(rf);
    let rounds = noop_rounds();

    match st.finalize(rf, "unwritten-summary", &rounds) {
        Err(StoreError::Stale) => {}
        other => panic!("finalize on an exactly-now lease must be Stale, got {other:?}"),
    }
    let snap = st.snapshot().unwrap();
    assert_eq!(snap.branches.len(), 1, "finalize: no branch added");
    assert!(
        snap.branches[0].rounds.is_empty(),
        "finalize: rounds unchanged"
    );
    assert_eq!(
        snap.branches[0].lifecycle,
        Lifecycle::Pending,
        "finalize: lifecycle unchanged"
    );
    assert_eq!(
        snap.branches[0].owner, rf.owner,
        "finalize: owner unchanged"
    );
    assert!(
        snap.branches[0].error.is_empty(),
        "finalize: error unchanged"
    );
    assert!(
        snap.branches[0].summary.is_empty(),
        "finalize: summary unchanged"
    );

    let st = store("eq-fail1");
    let rb = reserved(&st, Some(1), None, "P", &cwd).unwrap();
    set_lease_expiry(&st, &rb.branch_id, now_secs());
    let rb = leak_reserved(rb);
    let rounds = noop_rounds();
    match st.fail(rb, "unwritten-error", &rounds) {
        Err(StoreError::Stale) => {}
        other => panic!("fail on an exactly-now lease must be Stale, got {other:?}"),
    }
    let snap = st.snapshot().unwrap();
    assert_eq!(snap.branches.len(), 1, "fail: no branch added");
    assert!(snap.branches[0].rounds.is_empty(), "fail: rounds unchanged");
    assert_eq!(
        snap.branches[0].lifecycle,
        Lifecycle::Pending,
        "fail: lifecycle unchanged"
    );
    assert_eq!(snap.branches[0].owner, rb.owner, "fail: owner unchanged");
    assert!(snap.branches[0].error.is_empty(), "fail: error unchanged");
    assert!(
        snap.branches[0].summary.is_empty(),
        "fail: summary unchanged"
    );
}

/// A changed prompt at the same turn/prompt forks a new branch (the same old pending
/// child is not disturbed), and the fork parents to the predecessor turn.
#[test]
fn second_store_changed_prompt_forks_a_new_child_branch() {
    let cwd = new_cwd();
    let st1 = store("resv-fork");
    let st2 = store("resv-fork");
    complete_turn1(&st1, "P1", &cwd);
    let r2 = reserved(&st1, Some(2), None, "P2", &cwd).unwrap();

    let fork = reserved(&st2, Some(2), None, "P2b", &cwd).unwrap();
    assert!(!fork.replay, "a changed prompt is a fork, not a replay");
    assert!(!fork.retry);
    assert_ne!(fork.branch_id, r2.branch_id);
    let snap = st2.snapshot().unwrap();
    assert_eq!(snap.branches.len(), 3);
    let fb = snap
        .branches
        .iter()
        .find(|b| b.branch_id == fork.branch_id)
        .unwrap();
    assert_eq!(fb.lifecycle, Lifecycle::Pending);
    assert_eq!(fb.parent_branch.as_deref(), Some("b1"));
    // The original pending child is untouched.
    let old = snap
        .branches
        .iter()
        .find(|b| b.branch_id == r2.branch_id)
        .unwrap();
    assert_eq!(old.lifecycle, Lifecycle::Pending);
    assert_eq!(old.prompt, "P2");
}

/// A near-`MAX_BRANCHES` valid chain validates in place (linear pass) and a
/// near-`MAX_BRANCHES` cycle-shaped state is rejected as corruption. No wall-clock
/// assertion: only the outcome and the branch/tool caps.
#[test]
fn near_max_branches_chain_validates_and_cycle_shaped_state_is_corrupt() {
    use llxprt_code_rs::session::MAX_BRANCHES;

    let cwd = new_cwd();
    let root = shared_root();
    let dir = root.join("code-rs-sessions").join("resv-chain");
    std::fs::create_dir_all(&dir).unwrap();

    // Build a MAX_BRANCHES-long chain b1..bN with parent links so the turn +1
    // lineage contract is satisfied.
    let mut branches: Vec<BranchRecord> = (0..MAX_BRANCHES)
        .map(|i| branch((i as u32) + 1, 1, &format!("b{}", i + 1), "P"))
        .collect();
    for b in branches.iter_mut().skip(1) {
        let parent_seq = b.turn - 1;
        b.parent_branch = Some(format!("b{parent_seq}"));
        b.parent_turn = parent_seq;
        b.parent_attempt = 1;
    }
    let valid = SessionState {
        version: llxprt_code_rs::session::STORE_VERSION,
        session_id: "resv-chain".into(),
        cwd: Some(cwd.canonicalize().unwrap().to_string_lossy().to_string()),
        cwd_dev: workspace_identity(&cwd).0,
        cwd_ino: workspace_identity(&cwd).1,
        branches,
        next_branch_seq: MAX_BRANCHES as u64,
    };
    std::fs::write(
        dir.join("session.json"),
        serde_json::to_vec(&valid).unwrap(),
    )
    .unwrap();
    let store_ok = SessionStore::load(&SessionId::parse("resv-chain").unwrap()).unwrap();
    store_ok.snapshot().unwrap();

    // A chain cap violation is the early hard cap, not a panic and not a hang.
    let dir2 = root.join("code-rs-sessions").join("resv-cycle");
    std::fs::create_dir_all(&dir2).unwrap();
    let chain = store_ok.snapshot().unwrap();
    let mut cycle_state = chain;
    cycle_state.session_id = "resv-cycle".into();
    // A cycle-shaped graph is rejected as corruption (the +1 turn contract makes the
    // parent cycle itself inconsistent, so the rejection is always deterministic).
    let n = cycle_state.branches.len();
    cycle_state.branches[n - 1].parent_branch = Some("b1".to_string());
    std::fs::write(
        dir2.join("session.json"),
        serde_json::to_vec(&cycle_state).unwrap(),
    )
    .unwrap();
    let store_cy = SessionStore::load(&SessionId::parse("resv-cycle").unwrap()).unwrap();
    match store_cy.snapshot() {
        Err(StoreError::Corrupt(_)) => {}
        other => panic!("a cycle-shaped near-max state must be Corrupt, got {other:?}"),
    }

    // Over-cap: MAX_BRANCHES + 1 is rejected with the early "too many branches" cap.
    let dir = root.join("code-rs-sessions").join("resv-over");
    std::fs::create_dir_all(&dir).unwrap();
    let over = {
        let valid_again = SessionState {
            version: llxprt_code_rs::session::STORE_VERSION,
            session_id: "resv-over".into(),
            cwd: None,
            cwd_dev: 0,
            cwd_ino: 0,
            branches: cycle_state.branches.clone(),
            next_branch_seq: MAX_BRANCHES as u64,
        };
        // Append one more branch, pending with its own owner, so the early MAX_BRANCHES
        // cap fails before any per-branch validation.
        let mut s = valid_again;
        let mut extra = branch(1, 1, "zzz", "maybe");
        extra.owner = "x".to_string();
        extra.lifecycle = Lifecycle::Pending;
        s.branches.push(extra);
        s
    };
    std::fs::write(dir.join("session.json"), serde_json::to_vec(&over).unwrap()).unwrap();
    match SessionStore::load(&SessionId::parse("resv-over").unwrap()).and_then(|s| s.snapshot()) {
        Err(StoreError::Corrupt(m)) if m.contains("too many branches") => {}
        other => panic!("over-cap branches must be the early cap, got {other:?}"),
    }
}

#[test]
fn replacement_between_turns_is_rejected_by_workspace_identity() {
    let st = store("identity-between-turns");
    let cwd = new_cwd();
    complete_turn1(&st, "first", &cwd);
    let moved = cwd.with_extension("moved");
    std::fs::rename(&cwd, &moved).unwrap();
    std::fs::create_dir(&cwd).unwrap();

    let error = st.start_request(None, None, "second", &cwd).unwrap_err();
    assert!(
        matches!(error, StoreError::Invalid(message) if message == "session workspace identity changed")
    );
}

#[test]
fn replacement_after_capability_open_is_rejected_before_reservation() {
    let st = store("identity-before-reservation");
    let cwd = new_cwd();
    let retained = llxprt_code_rs::tools::WorkspaceCap::open(&cwd).unwrap();
    let moved = cwd.with_extension("moved");
    std::fs::rename(&cwd, &moved).unwrap();
    std::fs::create_dir(&cwd).unwrap();

    let error = st
        .start_request_with_workspace(None, None, "first", &cwd, &retained)
        .unwrap_err();
    assert!(
        matches!(error, StoreError::Invalid(message) if message == "workspace identity changed before reservation")
    );
    let first = st.start_request(None, None, "replacement", &cwd).unwrap();
    assert_eq!(first.branch_id, "b1");
}

#[test]
fn mismatched_agent_capability_is_rejected_before_backend_call() {
    let st = store("identity-before-model");
    let cwd = new_cwd();
    let reservation = st.start_request(None, None, "first", &cwd).unwrap();
    let moved = cwd.with_extension("moved");
    std::fs::rename(&cwd, &moved).unwrap();
    std::fs::create_dir(&cwd).unwrap();
    let agent = llxprt_code_rs::agent::CodingAgent::with_backend(
        Box::new(CountingBackend {
            calls: Mutex::new(0),
        }),
        cwd,
        false,
    );

    let error = agent.run(&st, &reservation).unwrap_err();
    assert_eq!(error.key, "session");
    assert_eq!(agent.model_calls(), 0);
}

/// Retrying a terminal operation across lifecycle classes is client misuse, not store
/// corruption. Same-owner same-state retries remain idempotent.
#[test]
fn cross_lifecycle_terminal_retries_are_invalid_not_corrupt() {
    let cwd = new_cwd();

    let completed = store("terminal-cross-completed");
    let completed_request = reserved(&completed, Some(1), None, "P", &cwd).unwrap();
    completed
        .finalize(&completed_request, "rejected", &noop_rounds())
        .unwrap();
    completed
        .finalize(&completed_request, "rejected", &noop_rounds())
        .expect("same completed terminal retry is idempotent");
    match completed.fail(&completed_request, "wrong lifecycle", &noop_rounds()) {
        Err(StoreError::Invalid(_)) => {}
        other => panic!("fail on Completed must be Invalid, got {other:?}"),
    }

    let failed = store("terminal-cross-failed");
    let failed_request = reserved(&failed, Some(1), None, "P", &cwd).unwrap();
    failed.fail(&failed_request, "failed", &[]).unwrap();
    failed
        .fail(&failed_request, "failed", &[])
        .expect("same failed terminal retry is idempotent");
    match failed.finalize(&failed_request, "wrong lifecycle", &[]) {
        Err(StoreError::Invalid(_)) => {}
        other => panic!("finalize on Failed must be Invalid, got {other:?}"),
    }
}

#[test]
fn stale_owner_cannot_retry_terminal_state_after_reclaim() {
    let cwd = new_cwd();
    let old_store = store("terminal-reclaimed-owner");
    let new_store = store("terminal-reclaimed-owner");
    let old = reserved(&old_store, Some(1), None, "P", &cwd).unwrap();
    expire_pending(&old_store);
    let reclaimed = reserved(&new_store, Some(1), None, "P", &cwd).unwrap();
    assert_ne!(old.owner, reclaimed.owner);
    new_store
        .finalize(&reclaimed, "rejected", &noop_rounds())
        .unwrap();
    new_store
        .finalize(&reclaimed, "rejected", &noop_rounds())
        .expect("same owner terminal retry remains idempotent");

    assert!(matches!(
        old_store.finalize(&old, "rejected", &noop_rounds()),
        Err(StoreError::Stale)
    ));
}
