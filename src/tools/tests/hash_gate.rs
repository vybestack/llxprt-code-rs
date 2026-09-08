//! `replace` `expected_sha256` diagnostics: syntax refusals, the distinct stale-digest
//! mismatch, and the exact recovery route. These run through the real registered tool
//! (`execute_tool`) with digests computed from real file bytes; no digest implementation is
//! mocked.

use super::*;

/// A SHA-256 over the *whole* file, matching `replace`'s documented digest. Computed with the
/// crate's own `digest_hex` (the same function the production checkpoint uses), so the tests
/// assert on real file content rather than a stub.
fn whole_file_digest(path: &std::path::Path) -> String {
    digest_hex(&std::fs::read(path).unwrap())
}

/// One staged fixture: a tempdir root plus the file name every call in this module uses.
struct Fixture {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    path: std::path::PathBuf,
}

fn stage(contents: &str) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gate.txt");
    std::fs::write(&path, contents).unwrap();
    let root = dir.path().to_path_buf();
    Fixture {
        _dir: dir,
        root,
        path,
    }
}

fn replace_call(f: &Fixture, old: &str, new: &str, hash: Option<&str>) -> (bool, String) {
    let mut args = json!({"path": "gate.txt", "old_string": old, "new_string": new});
    if let Some(h) = hash {
        args["expected_sha256"] = json!(h);
    }
    run(&f.root, "replace", args)
}

#[test]
fn empty_and_abbreviated_expected_sha256_are_syntax_refusals() {
    let fx = stage("alpha beta gamma\n");
    let before = std::fs::read(&fx.path).unwrap();
    let current = whole_file_digest(&fx.path);
    for bad in ["", &current[..16], &current[..63], "z", "0"] {
        let (ok, msg) = replace_call(&fx, "beta", "BETA", Some(bad));
        assert!(!ok, "an invalid expected_sha256 must block: {msg}");
        assert!(
            msg.contains("expected_sha256 is invalid"),
            "syntax refusal must be labelled invalid, got: {msg}"
        );
        assert!(
            !msg.contains("stale"),
            "a syntax refusal must not be called a mismatch: {msg}"
        );
        let expect_len = if bad.is_empty() {
            "empty".to_string()
        } else {
            format!("{} characters", bad.len())
        };
        let stated = msg.contains(&expect_len) || (bad.is_empty() && msg.contains("empty"));
        assert!(
            stated,
            "the refusal must state the supplied length ({expect_len}): {msg}"
        );
        assert!(
            msg.contains(&current),
            "the refusal must report the full current digest: {msg}"
        );
        assert_eq!(
            std::fs::read(&fx.path).unwrap(),
            before,
            "a syntax refusal must not write"
        );
    }
}

#[test]
fn wrong_length_nonhex_and_uppercase_values_are_syntax_refusals() {
    let fx = stage("one two three\n");
    let before = std::fs::read(&fx.path).unwrap();
    let current = whole_file_digest(&fx.path);
    let upper = current.to_uppercase();
    let nonhex = format!("{}zz", &current[..62]);
    let long = format!("{current}0");
    for bad in [&upper, nonhex.as_str(), long.as_str()] {
        let (ok, msg) = replace_call(&fx, "two", "2", Some(bad));
        assert!(!ok, "invalid expected_sha256 must block: {msg}");
        assert!(
            msg.contains("expected_sha256 is invalid"),
            "not a mismatch refusal: {msg}"
        );
        assert_eq!(
            std::fs::read(&fx.path).unwrap(),
            before,
            "an invalid digest must not change the file"
        );
    }
    // The uppercase case names the lowercase rule explicitly.
    let (_, msg) = replace_call(&fx, "two", "2", Some(&upper));
    assert!(
        msg.contains("lowercase"),
        "uppercase must be told to use lowercase: {msg}"
    );
}

#[test]
fn stale_full_digest_is_a_distinct_mismatch_with_the_current_digest() {
    let fx = stage("original content\n");
    let stale = whole_file_digest(&fx.path);
    std::fs::write(&fx.path, "revised content\n").unwrap();
    let current = whole_file_digest(&fx.path);
    let before = std::fs::read(&fx.path).unwrap();
    let (ok, msg) = replace_call(&fx, "revised", "REVISED", Some(&stale));
    assert!(!ok, "a stale full digest must block: {msg}");
    assert!(
        msg.contains("stale"),
        "the mismatch refusal must be distinguishable from syntax: {msg}"
    );
    assert!(
        !msg.contains("expected_sha256 is invalid"),
        "a valid stale digest is not a syntax error: {msg}"
    );
    assert!(
        msg.contains(&current),
        "the mismatch must report the full current digest: {msg}"
    );
    assert_eq!(
        std::fs::read(&fx.path).unwrap(),
        before,
        "a mismatch refusal must not write"
    );
    assert!(msg.contains("use read_file to re-read"), "{msg}");
    assert!(msg.contains("confirm old_string"), "{msg}");
    assert!(msg.contains("new_string"), "{msg}");
    assert!(
        msg.contains("do not omit expected_sha256"),
        "the route must not encourage bypassing the gate: {msg}"
    );
}

#[test]
fn matching_full_digest_replaces_and_prefixes_never_match() {
    let fx = stage("needle once\n");
    let current = whole_file_digest(&fx.path);
    let (ok, msg) = replace_call(&fx, "needle", "NEEDLE", Some(&current));
    assert!(ok, "a matching full digest must proceed: {msg}");
    assert_eq!(std::fs::read_to_string(&fx.path).unwrap(), "NEEDLE once\n");
    // The old prefix no longer matches the changed file, but even the *previous* full prefix
    // must be refused rather than accepted.
    std::fs::write(&fx.path, "needle again\n").unwrap();
    let now = whole_file_digest(&fx.path);
    let (ok, msg) = replace_call(&fx, "needle", "NEEDLE", Some(&now[..16]));
    assert!(!ok, "a 16-char prefix must never satisfy the gate: {msg}");
    assert!(
        msg.contains("expected_sha256 is invalid"),
        "prefix use must be a syntax refusal: {msg}"
    );
    assert_eq!(
        std::fs::read(&fx.path).unwrap(),
        b"needle again\n".to_vec(),
        "a prefix attempt must not write"
    );
}

/// The repeated-abbreviation loop: a model that gets a truncated digest in a refusal keeps
/// retrying with that abbreviation. The refusal must break the loop by reporting the *full*
/// current digest (or an exact route to it) and must never say an abbreviation is usable.
#[test]
fn repeated_abbreviation_sequence_recovers_with_the_full_digest() {
    let fx = stage("draft line\n");
    let first = whole_file_digest(&fx.path);
    // 1. The model supplies an abbreviation up front.
    let (ok, msg) = replace_call(&fx, "draft", "DRAFT", Some(&first[..16]));
    assert!(!ok);
    assert!(msg.contains("expected_sha256 is invalid"), "{msg}");
    // 2. It retries with the truncated digest it was previously shown by the old message shape.
    let mut seen = msg;
    for _ in 0..2 {
        // The abbreviated digest the model was last shown: an old-shaped message could have
        // surfaced a truncated tail, so the retry uses a 16-char prefix of the full digest.
        let prefix_source = whole_file_digest(&fx.path);
        let short = prefix_source[..16].to_string();
        let (ok, retry) = replace_call(&fx, "draft", "DRAFT", Some(&short));
        assert!(!ok, "the retry must still be refused: {retry}");
        seen = retry;
        assert!(seen.contains("expected_sha256 is invalid"), "{seen}");
    }
    // 3. Follow the diagnostic through the registered read tool and recheck both the current
    // content and intended edit before using the supplied full digest.
    let (read_ok, current_content) = run(
        &fx.root,
        "read_file",
        json!({"path": "gate.txt", "offset": 0, "limit": 11}),
    );
    assert!(
        read_ok,
        "the prescribed content recheck must work: {current_content}"
    );
    assert!(
        current_content.ends_with("draft line\n"),
        "{current_content}"
    );
    assert!(
        current_content.contains("draft"),
        "old_string must be reconfirmed"
    );
    let intended_replacement = "DRAFT";
    assert_ne!(
        intended_replacement, "draft",
        "replacement intent must be explicit"
    );
    let current = whole_file_digest(&fx.path);
    assert!(
        seen.contains(&current),
        "the refusal must contain the full current digest: {seen}"
    );
    let (ok, done) = replace_call(&fx, "draft", intended_replacement, Some(&current));
    assert!(ok, "recovery with the full digest must succeed: {done}");
    assert_eq!(std::fs::read_to_string(&fx.path).unwrap(), "DRAFT line\n");
    // 4. A prefix of the *matching* digest is still refused, proving no prefix acceptance.
    std::fs::write(&fx.path, "draft line\n").unwrap();
    let now = whole_file_digest(&fx.path);
    let (ok, msg) = replace_call(&fx, "draft", "DRAFT", Some(&now[..16]));
    assert!(!ok, "{msg}");
    assert!(msg.contains("expected_sha256 is invalid"), "{msg}");
    assert_eq!(std::fs::read_to_string(&fx.path).unwrap(), "draft line\n");
}
