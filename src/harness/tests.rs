use super::*;
use std::sync::Mutex;

mod publication;

/// The dsflash scenarios must carry the two explicit opt-ins so the spawned CLI can
/// reach the remote plaintext HTTP endpoint and register the shell tool.
#[test]
fn dsflash_invocation_args_pass_explicit_opt_ins() {
    let spec = InvocationSpec {
        session: "sess".into(),
        cwd: PathBuf::from("/tmp/ws"),
        prompt: "p".into(),
        turn: None,
        branch: None,
        profile: Some("dsflash-mi300x".into()),
        allow_insecure_http: true,
        allow_shell: true,
    };
    let args = spec.to_args();
    let has_profile = args
        .windows(3)
        .any(|w| w[0] == "--profile" && w[1] == "dsflash-mi300x");
    assert!(has_profile);
    assert!(args.contains(&"--allow-insecure-http".to_string()));
    assert!(args.contains(&"--allow-shell".to_string()));
    let has_prompt = args.windows(2).any(|w| w[0] == "-p" && w[1] == "p");
    assert!(has_prompt);
}

/// Opt-ins are off by default and not passed for a plain invocation.
#[test]
fn default_invocation_omits_opt_ins() {
    let spec = InvocationSpec {
        session: "s".into(),
        cwd: PathBuf::from("/tmp/ws"),
        prompt: "p".into(),
        turn: None,
        branch: None,
        profile: None,
        allow_insecure_http: false,
        allow_shell: false,
    };
    let args = spec.to_args();
    assert!(!args.contains(&"--allow-insecure-http".to_string()));
    assert!(!args.contains(&"--allow-shell".to_string()));
}

/// Config-dir selectors are carried into the child env; unrelated credential
/// variables the runner sees are never part of this list (the runner allow-list
/// scrubs them).
#[test]
fn config_env_add_carries_config_dirs_but_no_credentials() {
    use std::sync::Mutex;
    use std::sync::Once;
    static GUARD: Mutex<()> = Mutex::new(());
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        std::env::remove_var("FAKE_LLXPRT_CRED_9347");
    });
    let _g = GUARD.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::set_var("LLXPRT_CONFIG_HOME", "/tmp/llxprt-rs-config-test");
        std::env::remove_var("LLXPRT_CONFIG_DIR");
        std::env::set_var("FAKE_LLXPRT_CRED_9347", "super-secret");
    }
    let add = config_env_add();
    assert_eq!(
        add,
        vec![(
            "LLXPRT_CONFIG_HOME".to_string(),
            "/tmp/llxprt-rs-config-test".to_string()
        )]
    );
    assert!(
        !add.iter().any(|(k, _)| k.contains("CRED")),
        "credentials must never be added: {add:?}"
    );
    unsafe {
        std::env::remove_var("LLXPRT_CONFIG_HOME");
        std::env::remove_var("FAKE_LLXPRT_CRED_9347");
    }
}

fn spec(session: &str) -> InvocationSpec {
    InvocationSpec {
        session: session.into(),
        cwd: PathBuf::from("/tmp/ws"),
        prompt: "p".into(),
        turn: None,
        branch: None,
        profile: None,
        allow_insecure_http: false,
        allow_shell: false,
    }
}

fn ok_env(success: &str) -> OkEnvelope {
    serde_json::from_value(serde_json::json!({
        "session_id": success,
        "session_dir": "/tmp/sessions/sess",
        "turn": 1,
        "attempt": 1,
        "branch_id": "b1",
        "branch": false,
        "replayed": false,
        "summary": "done",
        "tool_calls": 3,
        "prompt_digest": crate::agent::prompt_digest("p"),
    }))
    .unwrap()
}

/// A valid success envelope passes the strict contract.
#[test]
fn ok_envelope_contract_passes() {
    let env: Envelope = serde_json::from_value(serde_json::json!({
        "session_id": "sess",
        "session_dir": "/tmp/sessions/sess",
        "turn": 1,
        "attempt": 1,
        "branch_id": "b1",
        "branch": false,
        "replayed": false,
        "status": "ok",
        "summary": "done",
        "tool_calls": 3,
        "prompt_digest": crate::agent::prompt_digest("p"),
    }))
    .unwrap();
    let mut r = test_result(true);
    fill(
        &mut r,
        &env,
        &spec("sess"),
        &mut ContinuationState::default(),
    )
    .unwrap();
    assert!(r.ok);
    assert_eq!(r.attempt, 1);
    assert_eq!(r.branch_id, "b1");
    assert_eq!(r.prompt_digest, crate::agent::prompt_digest("p"));
}

/// The exact **previously accepted** adversarial envelope: a success that the old
/// optional catch-all would have accepted (no session_dir, attempt 0) is now rejected
/// and the run is not ok.
/// The exact previously accepted adversarial success envelope (all the old required fields,
/// no session_dir, attempt 0) is rejected through the full parse-and-fill path with
/// `ok = false`; it is never a partial pass.
#[test]
fn previous_adversarial_envelope_ok_false() {
    let json = serde_json::json!({
        "session_id": "sess",
        "status": "ok",
        "turn": 1,
        "attempt": 0,
        "branch_id": "b1",
        "branch": false,
        "replayed": false,
        "summary": "done",
        "tool_calls": 3,
        "prompt_digest": crate::agent::prompt_digest("p")
    });
    let text = serde_json::to_string(&json).unwrap();
    // An envelope the old catch-all accepted (it contained every old-required field and
    // attempt 0) must now leave the run not-ok, whether it fails the typed parse or
    // the fill validation.
    match parse_one_object(text.as_bytes()) {
        Ok(env) => {
            let mut r = test_result(true);
            let e = fill(
                &mut r,
                &env,
                &spec("sess"),
                &mut ContinuationState::default(),
            );
            assert!(
                e.is_err(),
                "the previously accepted envelope must fail: {e:?}"
            );
            assert!(
                !r.ok,
                "ok must be false for the previously accepted envelope"
            );
        }
        Err(_) => {
            // A run over a rejected document never turns green.
            let r = BbResult::failed_spawn("parse rejected".to_string());
            assert!(!r.ok);
        }
    }
}

/// An unknown field is rejected by `deny_unknown_fields`: a success carrying a stray
/// `exit` key or an `error` object must fail the typed parse.
#[test]
fn ok_envelope_rejects_unknown_and_error_fields() {
    let mut good = serde_json::json!({
        "session_id": "sess",
        "session_dir": "/tmp/sessions/sess",
        "turn": 1,
        "attempt": 1,
        "branch_id": "b1",
        "branch": false,
        "replayed": false,
        "status": "ok",
        "summary": "done",
        "tool_calls": 3,
        "prompt_digest": crate::agent::prompt_digest("p"),
    });
    good["exit"] = serde_json::json!(0);
    let j = serde_json::to_string(&good).unwrap();
    let r: Result<Envelope, _> = serde_json::from_str(&j);
    assert!(r.is_err(), "a success with an extra field must be rejected");
    let mut with_err = serde_json::json!({
        "session_id": "sess",
        "session_dir": "/tmp/sessions/sess",
        "turn": 1,
        "attempt": 1,
        "branch_id": "b1",
        "branch": false,
        "replayed": false,
        "status": "ok",
        "summary": "done",
        "tool_calls": 3,
        "prompt_digest": crate::agent::prompt_digest("p"),
        "error": { "code": "x", "message": "y" },
    });
    with_err["error"] = serde_json::json!({"code":"x","message":"y"});
    let j = serde_json::to_string(&with_err).unwrap();
    let r: Result<Envelope, _> = serde_json::from_str(&j);
    assert!(
        r.is_err(),
        "a success carrying an error object must be rejected"
    );
}

/// A success must report the expected turn; a mismatch is rejected.
#[test]
fn ok_envelope_wrong_turn_is_rejected() {
    let mut r = test_result(false);
    let env = spec("sess");
    let mut e = ok_env("sess");
    e.turn = 2;
    let e = Envelope::Ok(e);
    let err = fill(&mut r, &e, &env, &mut ContinuationState::default()).unwrap_err();
    assert!(err.contains("expected turn 1, got 2"), "{err}");
    assert!(!r.ok);
}
/// A mismatched digest (wrong prompt) is rejected against the independently computed
/// FNV-1a digest of the submitted prompt.
#[test]
fn ok_envelope_wrong_digest_is_rejected() {
    let mut r = test_result(false);
    let env = spec("sess");
    let mut e = ok_env("sess");
    e.prompt_digest = crate::agent::prompt_digest("completely different");
    let e = Envelope::Ok(e);
    let err = fill(&mut r, &e, &env, &mut ContinuationState::default()).unwrap_err();
    assert!(err.contains("prompt_digest mismatch"), "{err}");
    assert!(!r.ok);
}

/// An empty session_dir / branch_id / prompt_digest is rejected.
#[test]
fn ok_envelope_empty_required_strings_are_rejected() {
    for (field, value) in [
        ("session_dir", ""),
        ("session_dir", "   "),
        ("branch_id", ""),
        ("prompt_digest", ""),
    ] {
        let mut r = test_result(false);
        let env = spec("sess");
        let mut e = ok_env("sess");
        match field {
            "session_dir" => {
                if value == "   " {
                    e.session_dir = value.to_string();
                } else {
                    e.session_dir.clear();
                }
            }
            "branch_id" => e.branch_id = value.to_string(),
            "prompt_digest" => e.prompt_digest = value.to_string(),
            _ => unreachable!(),
        }
        let e = Envelope::Ok(e);
        assert!(
            fill(&mut r, &e, &env, &mut ContinuationState::default()).is_err(),
            "{field} empty must be rejected"
        );
        assert!(!r.ok);
    }
}

/// attempt must be >= 1.
#[test]
fn ok_envelope_zero_attempt_is_rejected() {
    let mut r = test_result(false);
    let env = spec("sess");
    let mut e = ok_env("sess");
    e.attempt = 0;
    let e = Envelope::Ok(e);
    assert!(fill(&mut r, &e, &env, &mut ContinuationState::default()).is_err());
    assert!(!r.ok);
}

/// `--branch` names a previously returned parent. The continued turn returns a
/// distinct child branch id, while an unknown parent is rejected.
#[test]
fn continuation_requires_an_observed_parent_and_accepts_a_distinct_child() {
    let mut state = ContinuationState::default();
    let mut first = test_result(false);
    first.exit = Some(0);
    fill(
        &mut first,
        &Envelope::Ok(ok_env("sess")),
        &spec("sess"),
        &mut state,
    )
    .unwrap();
    assert_eq!(state.branch_ids, ["b1"]);

    let mut invocation = spec("sess");
    invocation.turn = Some(2);
    invocation.branch = Some("missing".to_string());
    invocation.prompt = "child".to_string();
    let mut child = ok_env("sess");
    child.turn = 2;
    child.branch = true;
    child.branch_id = "b2".to_string();
    child.prompt_digest = crate::agent::prompt_digest("child");
    let mut result = test_result(false);
    result.exit = Some(0);
    let error = fill(
        &mut result,
        &Envelope::Ok(child.clone()),
        &invocation,
        &mut state,
    )
    .unwrap_err();
    assert!(error.contains("was not returned by an earlier validated turn"));

    invocation.branch = Some("b1".to_string());
    fill(&mut result, &Envelope::Ok(child), &invocation, &mut state).unwrap();
    assert!(result.ok);
    assert_eq!(result.branch_id, "b2");
    assert_eq!(state.branch_ids, ["b1", "b2"]);
}

/// Exit/status disagreement: ok with a nonzero exit is rejected.
#[test]
fn ok_status_with_nonzero_exit_is_rejected() {
    let mut r = test_result(false);
    r.exit = Some(7);
    let e = Envelope::Ok(ok_env("sess"));
    let err = fill(&mut r, &e, &spec("sess"), &mut ContinuationState::default()).unwrap_err();
    assert!(err.contains("disagrees"), "{err}");
    assert!(!r.ok);
}

/// A success whose subprocess output was not fully captured can never be a protocol
/// pass: truncated stdout, any combined over-cap, a deadline hit, or a signal death
/// all reject the run even when the envelope itself is otherwise perfect. The raw
/// artifacts are preserved (they live on the result), only `ok` stays false.
#[test]
fn ok_envelope_rejects_truncated_timed_out_or_signalled_output() {
    enum Capture {
        StdoutTruncated,
        CombinedTruncated,
        TimedOut,
        Signal,
    }
    for cap in [
        Capture::StdoutTruncated,
        Capture::CombinedTruncated,
        Capture::TimedOut,
        Capture::Signal,
    ] {
        let mut r = test_result(false);
        r.exit = Some(0);
        let label = match cap {
            Capture::StdoutTruncated => "stdout_truncated",
            Capture::CombinedTruncated => "combined_truncated",
            Capture::TimedOut => "timed_out",
            Capture::Signal => "signal",
        };
        match cap {
            Capture::StdoutTruncated => r.stdout_truncated = true,
            Capture::CombinedTruncated => r.combined_truncated = true,
            Capture::TimedOut => r.timed_out = true,
            Capture::Signal => r.exit = None,
        }
        let e = Envelope::Ok(ok_env("sess"));
        let err = fill(&mut r, &e, &spec("sess"), &mut ContinuationState::default()).unwrap_err();
        assert!(
            err.contains("not fully captured"),
            "{label} must reject: {err}"
        );
        assert!(!r.ok, "{label} must leave ok false");
        // The raw capture is preserved on the result for artifact files.
        assert_eq!(r.raw_stdout.len(), 0);
    }
}

/// A success envelope reporting more tool calls than one attempt could run (the
/// `u64::MAX` case) is rejected on every host: the `try_from` never panics and
/// the attempt-budget gate turns the run red, never a pass.
#[test]
fn ok_envelope_rejects_tool_calls_over_the_attempt_budget() {
    let mut r = test_result(false);
    r.exit = Some(0);
    let mut e = ok_env("sess");
    e.tool_calls = u64::MAX;
    let e = Envelope::Ok(e);
    let err = fill(&mut r, &e, &spec("sess"), &mut ContinuationState::default()).unwrap_err();
    assert!(err.contains("exceeds the per-turn budget"), "{err}");
    assert!(!r.ok);
}

/// An error envelope without error detail, or with a zero exit, is rejected, and an
/// error carrying success fields is rejected by the typed contract.
#[test]
fn error_envelope_requires_detail_and_nonzero_exit() {
    // Bare status error, no detail: parse fails.
    let env: Result<Envelope, _> = serde_json::from_str(r#"{"status":"error"}"#);
    assert!(env.is_err(), "error without detail must not parse");
    // Error with a success field is rejected by deny_unknown_fields.
    let env: Result<Envelope, _> =
        serde_json::from_str(r#"{"status":"error","error":{"code":"e","message":"m"},"turn":1}"#);
    assert!(
        env.is_err(),
        "error carrying a success field must be rejected"
    );
    // Error with exit 0 disagrees.
    let mut r = test_result(false);
    r.exit = Some(0);
    let env: Envelope = serde_json::from_str(
        r#"{"session_id":"sess","status":"error","error":{"code":"model","message":"boom"}}"#,
    )
    .unwrap();
    let err = fill(
        &mut r,
        &env,
        &spec("sess"),
        &mut ContinuationState::default(),
    )
    .unwrap_err();
    assert!(err.contains("disagrees"), "{err}");
    // A proper error envelope with nonzero exit fills the typed fields.
    let mut r = test_result(false);
    r.exit = Some(5);
    fill(
        &mut r,
        &env,
        &spec("sess"),
        &mut ContinuationState::default(),
    )
    .unwrap();
    assert_eq!(r.status, "error");
    assert_eq!(r.error_code, "model");
    assert_eq!(r.error_message, "boom");
    assert!(!r.ok);
}

/// An ok envelope missing a required success field fails the typed parse (a missing
/// field is a contract failure, never a partial pass).
#[test]
fn ok_envelope_missing_required_field_is_rejected() {
    let without = serde_json::json!({
        "session_id": "sess", "status": "ok"
    });
    let env: Result<Envelope, _> = serde_json::from_value(without);
    assert!(env.is_err(), "status-only ok must fail the typed parse");
}

/// A wrong JSON type in a present field fails the typed parse.
#[test]
fn wrong_field_type_fails_the_typed_parse() {
    let env: Result<Envelope, _> =
        serde_json::from_str(r#"{"session_id":"s","status":"ok","turn":"abc"}"#);
    assert!(env.is_err(), "a string turn must fail the typed parse");
    let env: Result<Envelope, _> =
        serde_json::from_str(r#"{"session_id":"s","status":"ok","branch_id":{"a":1}}"#);
    assert!(
        env.is_err(),
        "an object branch_id must fail the typed parse"
    );
}
#[test]
fn trailing_or_multiple_json_is_rejected_by_parse() {
    assert!(parse_one_object(br#"{"status":"ok"}{"status":"ok"}"#).is_err());
    assert!(parse_one_object(br#"{"status":"ok"}  garbage"#).is_err());
    assert!(parse_one_object(b"").is_err());
    assert!(parse_one_object(&[0xff, 0xfe]).is_err());
    // A standalone well-formed complete success parses (whitespace-padded) and the
    // strict per-status validation still demands the full contract.
    let full = serde_json::json!({
        "session_id": "sess",
        "session_dir": "/tmp/sessions/sess",
        "turn": 1,
        "attempt": 1,
        "branch_id": "b1",
        "branch": false,
        "replayed": false,
        "status": "ok",
        "summary": "done",
        "tool_calls": 3,
        "prompt_digest": crate::agent::prompt_digest("p")
    });
    let padded = format!("  {full}  ");
    match parse_one_object(padded.as_bytes()) {
        Ok(Envelope::Ok(e)) => {
            let mut r = test_result(false);
            r.exit = Some(0);
            fill(
                &mut r,
                &Envelope::Ok(e),
                &spec("sess"),
                &mut ContinuationState::default(),
            )
            .unwrap();
            assert!(r.ok);
        }
        _ => panic!("a single padded complete envelope must parse"),
    }
}

/// Follow-ups must stop after the first failure.
#[test]
fn run_turns_aborts_followups_after_first_failure() {
    let calls: Mutex<Vec<(String, Option<u32>)>> = Mutex::new(Vec::new());
    let s = Scenario {
        name: "t".into(),
        prompt: "p0".into(),
        max_turns: 4,
        follows: vec!["p1", "p2", "p3"],
    };
    let results = run_turns(&s, |p, t| {
        calls.lock().unwrap().push((p.to_string(), t));
        // first turn ok, every follow-up fails.
        Ok(test_result(t.is_none()))
    })
    .unwrap();
    let seq = calls.lock().unwrap().clone();
    assert_eq!(seq.len(), 2, "third follow-up must not run: {seq:?}");
    assert_eq!(seq[0], ("p0".to_string(), None));
    assert_eq!(seq[1], ("p1".to_string(), Some(2)));
    assert_eq!(results.len(), 2);
    assert!(results[0].ok);
    assert!(!results[1].ok);
}

/// A turn whose artifacts cannot be saved propagates `Err` instead of continuing.
#[test]
fn run_turns_propagates_save_failures() {
    let s = Scenario {
        name: "t".into(),
        prompt: "p0".into(),
        max_turns: 4,
        follows: vec!["p1"],
    };
    let e = run_turns(&s, |_, _| -> Result<BbResult, String> {
        Err("disk full".to_string())
    })
    .unwrap_err();
    assert!(e.contains("disk full"));
}

/// max_turns bounds the number of follow-ups even when every turn succeeds.
#[test]
fn run_turns_respects_max_turns_budget() {
    let calls: Mutex<Vec<Option<u32>>> = Mutex::new(Vec::new());
    let s = Scenario {
        name: "t".into(),
        prompt: "p0".into(),
        max_turns: 2,
        follows: vec!["p1", "p2", "p3"],
    };
    let _ = run_turns(&s, |_, t| {
        calls.lock().unwrap().push(t);
        Ok(test_result(true))
    })
    .unwrap();
    assert_eq!(
        calls.lock().unwrap().len(),
        2,
        "max_turns must cap follow-ups"
    );
}

/// Artifacts preserve the exact stdout/stderr bytes (including invalid UTF-8) and the
/// typed meta with truncation flags; neither stream is trimmed.
#[test]
fn save_turn_preserves_exact_bytes_and_flags() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = test_result(true);
    r.raw_stdout = b"{\"status\":\"ok\"}  \n".to_vec();
    r.stderr = b"progress line\n".to_vec();
    r.stdout_truncated = false;
    r.stderr_truncated = true;
    r.combined_truncated = true;
    save_turn(dir.path(), "starter", "sess", 1, &r).unwrap();
    let out = fs::read(dir.path().join("starter/sess.turn1.json")).unwrap();
    assert_eq!(&out, b"{\"status\":\"ok\"}  \n");
    let err = fs::read(dir.path().join("starter/sess.turn1.stderr")).unwrap();
    assert_eq!(&err, b"progress line\n");
    let meta: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.path().join("starter/sess.turn1.meta.json")).unwrap())
            .unwrap();
    assert_eq!(meta["turn"], 1);
    assert_eq!(meta["ok"], true);
    assert_eq!(meta["stderr_truncated"], true);
    assert_eq!(meta["combined_truncated"], true);
    assert_eq!(meta["stdout_truncated"], false);
}

/// Invalid UTF-8 stdout bytes survive the artifact file verbatim.
#[test]
fn save_turn_preserves_invalid_utf8_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = test_result(false);
    r.raw_stdout = vec![0xff, 0xfe, 0x80, b'o', b'k'];
    r.stderr = vec![0xfd];
    save_turn(dir.path(), "starter", "sess", 1, &r).unwrap();
    let out = fs::read(dir.path().join("starter/sess.turn1.json")).unwrap();
    assert_eq!(out, vec![0xff, 0xfe, 0x80, b'o', b'k']);
    let err = fs::read(dir.path().join("starter/sess.turn1.stderr")).unwrap();
    assert_eq!(err, vec![0xfd]);
}

/// A symlinked directory appears as an entry but its contents are not followed.
#[test]
fn inventory_lists_symlink_without_following() {
    let d = tempfile::tempdir().unwrap();
    fs::create_dir_all(d.path().join("real")).unwrap();
    fs::write(d.path().join("real/secret.txt"), "x").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(d.path().join("real"), d.path().join("link")).unwrap();
    let inv = inventory(d.path());
    assert!(inv.files.iter().any(|i| i == "real"), "real dir is listed");
    assert!(
        inv.files.iter().any(|i| i == "real/secret.txt"),
        "real contents are listed: {:?}",
        inv.files
    );
    assert!(
        !inv.files.iter().any(|i| i.starts_with("link/")),
        "symlinked dir contents must not be followed: {:?}",
        inv.files
    );
}

#[cfg(unix)]
#[test]
fn inventory_directory_to_symlink_swap_never_lists_outside_names() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("victim")).unwrap();
    fs::write(root.path().join("victim/inside.txt"), "inside").unwrap();
    fs::write(outside.path().join("outside.txt"), "outside").unwrap();

    let mut swapped = false;
    let inv = inventory_inner(root.path(), |workspace, rel| {
        if !swapped && rel == "victim" {
            fs::rename(workspace.join("victim"), workspace.join("moved")).unwrap();
            std::os::unix::fs::symlink(outside.path(), workspace.join("victim")).unwrap();
            swapped = true;
        }
    });

    assert!(swapped, "the deterministic swap seam was not reached");
    assert!(
        !inv.files.iter().any(|path| path == "victim/outside.txt"),
        "descriptor-relative no-follow traversal escaped: {:?}",
        inv.files
    );
}

/// A large tree (more entries than the item cap) proves bounded traversal: the
/// inventory stops at the cap instead of visiting the whole tree, still returns a
/// sorted deterministic slice, and records the explicit `truncated` metadata.
#[test]
fn large_tree_inventory_stops_at_cap_stays_sorted_and_marks_truncated() {
    let d = tempfile::tempdir().unwrap();
    let sub = d.path().join("big");
    std::fs::create_dir_all(&sub).unwrap();
    let total = MAX_INVENTORY_ITEMS + 500;
    for i in 0..total {
        std::fs::write(sub.join(format!("f{i:05}")), "x").unwrap();
    }
    let inv = inventory(d.path());
    assert!(
        inv.truncated,
        "a tree larger than the item cap must be marked truncated"
    );
    assert!(
        inv.files.len() <= MAX_INVENTORY_ITEMS,
        "the inventory must stop at the cap, not collect the whole tree: {}",
        inv.files.len()
    );
    assert!(
        inv.files.len() < total,
        "must not have visited every entry: {} of {total}",
        inv.files.len()
    );
    // Sorted deterministic output on the truncation path.
    let mut sorted = inv.files.clone();
    sorted.sort();
    assert_eq!(inv.files, sorted, "truncated inventory must be sorted");
    let inv2 = inventory(d.path());
    assert_eq!(
        inv.files, inv2.files,
        "truncated inventory is deterministic"
    );
    // The same cap marks a repeated run truncated too, so the truncation metadata is
    // explicit and the output never silently drops the tail.
    assert!(inv2.truncated);
}

/// The main CLI and the harness must derive the prompt digest from exactly one shared
/// function (`crate::agent::prompt_digest`). A `CompletedRun` the CLI
/// serializes carries that digest, and the harness accepts the very same digest for the
/// same prompt — there is no second implementation whose drift could pass a CLI byte-for-byte
/// while the harness computes something else.
#[test]
fn cli_and_harness_share_one_prompt_digest() {
    use crate::agent::CompletedRun;
    use crate::cli::{self, RunOutcome};
    use crate::session::SessionId;
    for prompt in ["", "short", "Pong identity fixture", "filecrypt fixture"] {
        let digest = crate::agent::prompt_digest(prompt);
        let outcome = RunOutcome {
            session: SessionId::parse("s").unwrap(),
            session_dir: std::path::PathBuf::from("/config/code-rs-sessions/s"),
            run: CompletedRun {
                turn: 1,
                attempt: 1,
                branch_id: "b1".into(),
                summary: "done".into(),
                tool_count: 3,
                prompt_digest: digest.clone(),
                status: "ok".into(),
                branch: false,
                replayed: false,
            },
        };
        assert_eq!(
            cli::to_json(&Ok(outcome))["prompt_digest"],
            serde_json::Value::String(digest.clone()),
            "the CLI serializes the shared digest for {prompt:?}"
        );
        // The harness validates the identical digest for the identical prompt.
        let env: Envelope = serde_json::from_value(serde_json::json!({
            "session_id": "s", "session_dir": "/tmp/sessions/s",
            "turn": 1, "attempt": 1, "branch_id": "b1", "branch": false,
            "replayed": false, "status": "ok", "summary": "done", "tool_calls": 3,
            "prompt_digest": digest,
        }))
        .unwrap();
        let mut r = test_result(false);
        r.exit = Some(0);
        let spec = InvocationSpec {
            session: "s".into(),
            cwd: PathBuf::from("/tmp/ws"),
            prompt: prompt.to_string(),
            turn: None,
            branch: None,
            profile: None,
            allow_insecure_http: false,
            allow_shell: false,
        };
        fill(&mut r, &env, &spec, &mut ContinuationState::default()).unwrap();
        assert!(r.ok, "harness accepts the shared digest for {prompt:?}");
    }
}
