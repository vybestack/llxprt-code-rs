//! Issue 243: the model-visible `replace` refusal under shrinking aggregate turn budget.
//! The digest a refusal advertises must arrive whole or not at all — through the real
//! agent round, the registered tool dispatch, post-scrub truncation, the budget-notice
//! reservation, and the persisted transcript record.

use super::*;
use sha2::{Digest as _, Sha256};

/// The workspace body every call here replaces inside, and its whole-file digest.
const BODY: &str = "agent budget body\n";

/// Both cut markers the pipeline can leave in a rendered refusal: the registered tool
/// renderer's `...  [truncated N bytes]  ` and the post-scrub `[truncated]`, either of
/// which may arrive as a prefix when even the marker cannot fit.
const MARKER_HEADS: [&str; 2] = ["...  [", "[truncated"];

fn body_digest() -> String {
    let mut h = Sha256::new();
    h.update(BODY.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// One scripted turn: a single `replace` tool call, then a terminal summary. The agent's
/// per-turn output cap is `turn_cap`, so the refusal renders under the exact remaining
/// budget arithmetic the loop applies — including the reserved budget notice. The run
/// itself is kept as the `Result` it really is: a turn with no room for the rendered
/// result is a typed `AgentError`, never a faked `CompletedRun`.
struct Turn {
    run: Result<CompletedRun, AgentError>,
    model_calls: usize,
    store: SessionStore,
    target: std::path::PathBuf,
    _cwd: tempfile::TempDir,
}

fn run_refusal_turn(turn_cap: usize, expected_sha256: &str) -> Turn {
    let cwd = tempfile::tempdir().unwrap();
    let target = cwd.path().join("gate.txt");
    std::fs::write(&target, BODY).unwrap();
    let id = SessionId::fresh();
    let config = shared_config_home().join(&id.id);
    let store = SessionStore::load_at(&id, &config).unwrap();
    let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    let tool_round = LlmResult {
        text: String::new(),
        calls: vec![ToolCall {
            id: "c1".into(),
            name: "replace".into(),
            args_json: format!(
                "{{\"path\":\"gate.txt\",\"old_string\":\"budget\",\"new_string\":\"BUDGET\",\
                 \"expected_sha256\":\"{expected_sha256}\"}}"
            ),
        }],
        finish_reason: Some(FinishReason::ToolCall),
        usage: LlmUsage::default(),
    };
    let final_round = LlmResult {
        text: "done".into(),
        calls: Vec::new(),
        finish_reason: Some(FinishReason::Stop),
        usage: LlmUsage::default(),
    };
    let agent = CodingAgent::with_backend(
        Box::new(MockBackend::new(vec![tool_round, final_round])),
        cwd.path().to_path_buf(),
        false,
    )
    .with_max_tool_calls(Some(16))
    .with_output_caps(crate::envelope::OutputCaps {
        shell: 32 * 1024,
        tool: 16 * 1024,
        turn: turn_cap,
    });
    let run = agent.run(&store, &reserved);
    let model_calls = agent.model_calls();
    Turn {
        run,
        model_calls,
        store,
        target,
        _cwd: cwd,
    }
}

/// The retained body of a rendered refusal: the bytes still identical to the uncut
/// rendering, stopped at the first cut marker of either form. Splitting on one marker
/// form alone would count the other form's marker bytes — and the reserved budget
/// notice after them — as retained body, which is how prose-shaped suffix length came
/// to be mistaken for carried digest bytes.
fn retained_body_len(msg: &str, uncut: &str) -> usize {
    let shared = msg
        .as_bytes()
        .iter()
        .zip(uncut.as_bytes())
        .take_while(|(kept, was)| kept == was)
        .count();
    let marker = MARKER_HEADS
        .iter()
        .filter_map(|head| msg.find(head))
        .min()
        .unwrap_or(msg.len());
    shared.min(marker)
}

/// How many leading bytes of the digest token survived into a rendering that does not
/// carry the whole token. `None` means whole or wholly absent; `Some(k)` means a
/// `k`-character usable-looking abbreviation reached the model.
fn digest_prefix_bytes(msg: &str, uncut: &str, digest: &str, at: usize) -> Option<usize> {
    if msg.contains(digest) {
        return None;
    }
    retained_body_len(msg, uncut)
        .checked_sub(at)
        .filter(|carried| *carried > 0)
}

/// The field is either exactly the full digest or absent. Anchored on the field's own
/// offset in the uncut rendering: a naive `contains(prefix)` sweep starting at one
/// character would trip over ordinary prose sharing single hex characters with the digest.
fn assert_digest_whole_or_absent(uncut: &str, msg: &str, digest: &str, label: &str) {
    let Some(at) = uncut.find(digest) else {
        return;
    };
    if let Some(carried) = digest_prefix_bytes(msg, uncut, digest, at) {
        panic!("{label}: a {carried}-character digest fragment reached the model: {msg}");
    }
}

/// A turn with no room to run anything at all must fail with the typed output-cap
/// refusal rather than be forced into an impossible success.
#[test]
fn an_exhausted_turn_budget_fails_with_the_typed_output_cap_refusal() {
    let digest = body_digest();
    let turn = run_refusal_turn(1, &digest[..16]);
    let error = turn
        .run
        .expect_err("a 1-byte turn budget must fail, not fake a completed run");
    assert_eq!(error.key, "turn-budget");
    assert_eq!(error.code, crate::envelope::Code::Model);
    assert!(
        error
            .message
            .contains("turn tool output would exceed the 1 byte cap"),
        "{}",
        error.message
    );
    // The refusal did run, but its zero-byte body budget leaves only the independently
    // reserved notice. The aggregate cap check then fails before another provider call,
    // so this oversized accounting notice is recorded as failure evidence but is never
    // sent back to the model.
    assert_eq!(
        turn.model_calls, 1,
        "the failed path must stop before a subsequent provider request"
    );
    let state = turn.store.snapshot().unwrap();
    let branch = &state.branches[0];
    assert_eq!(branch.rounds.len(), 1);
    assert_eq!(branch.rounds[0].calls.len(), 1);
    let recorded = &branch.rounds[0].calls[0];
    assert!(!recorded.ok, "the replace guard must refuse the edit");
    assert!(
        !recorded.refused,
        "the registered tool ran; this is not a zero-execution budget refusal"
    );
    assert_eq!(recorded.result, "\n\n[budget: 15 of 16 tool calls left]");
    assert!(recorded.result.len() > 1);
    assert!(!recorded.result.contains(&digest));
    assert_eq!(branch.lifecycle, Lifecycle::Failed);
    assert!(
        branch
            .error
            .contains("turn tool output would exceed the 1 byte cap"),
        "{}",
        branch.error
    );
    assert_eq!(
        std::fs::read_to_string(&turn.target).unwrap(),
        BODY,
        "an exhausted budget must not write"
    );
}

#[test]
fn agent_refusal_digest_is_whole_or_omitted_under_a_shrinking_turn_budget() {
    let digest = body_digest();
    // The budget notice for the last (only) call of the round is reserved out of the
    // remaining budget before the tool renders, so these caps exercise the composed
    // boundary: inner tool cap, notice reservation, and post-scrub truncation.
    let Turn {
        run,
        store: uncut_store,
        ..
    } = run_refusal_turn(16 * 1024, &digest[..16]);
    run.expect("the uncut turn must complete");
    let uncut = uncut_store.snapshot().unwrap().branches[0].rounds[0].calls[0]
        .result
        .clone();
    // Every cut inside the digest itself, including the 1..7-byte cuts below any guessed
    // "plausible prefix" threshold, at the notice-reserved boundary.
    let digest_at = uncut
        .find(&digest)
        .expect("the uncut rendering carries the digest");
    let caps = [
        40usize, 96, 160, 224, 288, 352, 416, 480, 544, 608, 700, 1024, 4096,
    ];
    let exhaustive: Vec<usize> = (digest_at + 1..=digest_at + 64).collect();
    for turn_cap in caps.into_iter().chain(exhaustive) {
        let Turn {
            run,
            store,
            target,
            _cwd,
            ..
        } = run_refusal_turn(turn_cap, &digest[..16]);
        let run = run.unwrap_or_else(|error| panic!("turn cap {turn_cap} failed: {error}"));
        let state = store.snapshot().unwrap();
        let seen = state.branches[0].rounds[0].calls[0].result.as_str();
        assert!(
            seen.len() <= turn_cap,
            "turn cap {turn_cap}: rendered {} bytes: {seen}",
            seen.len()
        );
        assert_digest_whole_or_absent(
            uncut.as_str(),
            seen,
            &digest,
            &format!("turn cap {turn_cap}"),
        );
        // The reserved notice survives its own reservation.
        assert!(
            seen.contains("tool calls left"),
            "turn cap {turn_cap}: the reserved notice is missing: {seen}"
        );
        // The refusal never writes.
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            BODY,
            "turn cap {turn_cap}: a refusal must not write"
        );
        assert_eq!(run.tool_count, 1, "turn cap {turn_cap}");
    }
}

/// The stale branch is a different refusal text with the digest in a different position;
/// the invariant is the same.
#[test]
fn agent_stale_refusal_digest_is_whole_or_omitted() {
    let digest = body_digest();
    let mut stale = Sha256::new();
    stale.update(b"prior workspace bytes\n");
    let stale_digest: String = stale
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let uncut = {
        let Turn { run, store, .. } = run_refusal_turn(16 * 1024, &stale_digest);
        run.expect("the uncut turn must complete");
        store.snapshot().unwrap().branches[0].rounds[0].calls[0]
            .result
            .clone()
    };
    for turn_cap in [64usize, 144, 240, 336, 432, 528, 624, 768, 1024] {
        let Turn {
            run,
            store,
            target,
            _cwd,
            ..
        } = run_refusal_turn(turn_cap, &stale_digest);
        run.unwrap_or_else(|error| panic!("stale at {turn_cap} failed: {error}"));
        let state = store.snapshot().unwrap();
        let seen = state.branches[0].rounds[0].calls[0].result.as_str();
        assert!(seen.len() <= turn_cap, "turn cap {turn_cap}: {seen}");
        assert_digest_whole_or_absent(
            uncut.as_str(),
            seen,
            &digest,
            &format!("stale at {turn_cap}"),
        );
        assert_digest_whole_or_absent(
            uncut.as_str(),
            seen,
            &stale_digest,
            &format!("stale at {turn_cap}"),
        );
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            BODY,
            "turn cap {turn_cap}: a stale refusal must not write"
        );
    }
}

/// The assertion itself is sensitive at every genuinely partial length 1..63 against the
/// real uncut rendering, and does not fire on prose that merely shares single hex
/// characters with the digest (the reason the check is anchored on the field's offset).
#[test]
fn the_digest_assertion_is_sensitive_to_every_partial_length() {
    let digest = body_digest();
    let uncut = {
        let Turn { run, store, .. } = run_refusal_turn(16 * 1024, &digest[..16]);
        run.expect("the uncut turn must complete");
        store.snapshot().unwrap().branches[0].rounds[0].calls[0]
            .result
            .clone()
    };
    let at = uncut
        .find(&digest)
        .expect("the uncut rendering carries the digest");
    let notice = "\n\n[budget: 15 of 16 tool calls left]";
    for carried in 1..64usize {
        let partial = format!(
            "{}{}{}",
            &uncut[..at + carried],
            crate::redact::TRUNCATION_MARKER,
            notice
        );
        assert_eq!(
            digest_prefix_bytes(&partial, &uncut, &digest, at),
            Some(carried),
            "a {carried}-character fragment must be recognized"
        );
    }
    // Backing off to the field's start — the production behavior — reads as absent, and
    // so does every shorter cut, whatever prose precedes the field.
    for stop in [0, 1, at.saturating_sub(1), at] {
        let prose = format!(
            "{}{}{}",
            &uncut[..stop],
            crate::redact::TRUNCATION_MARKER,
            notice
        );
        assert_eq!(
            digest_prefix_bytes(&prose, &uncut, &digest, at),
            None,
            "a body stopping {stop} bytes in carries no fragment"
        );
    }
}

#[test]
fn agent_refusal_keeps_the_full_route_when_the_budget_is_adequate() {
    let digest = body_digest();
    let Turn { run, store, .. } = run_refusal_turn(16 * 1024, &digest[..16]);
    let run = run.expect("an adequate budget must complete the turn");
    let state = store.snapshot().unwrap();
    let seen = state.branches[0].rounds[0].calls[0].result.as_str();
    assert!(
        seen.contains(&digest),
        "the full digest must survive: {seen}"
    );
    assert!(seen.contains("use read_file to re-read"), "{seen}");
    assert!(seen.contains("confirm old_string"), "{seen}");
    assert!(seen.contains("do not omit expected_sha256"), "{seen}");
    assert!(seen.contains("tool calls left"), "{seen}");
    assert_eq!(run.tool_count, 1);
}
