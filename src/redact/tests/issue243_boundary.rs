//! Issue 243 regression: a truncation boundary must never hand the model a fragment of a
//! digest token. The rule is anchored on the real 64-character sha256 token the `replace`
//! guard mints, not on a guessed "plausible prefix" length. Assertions are anchored on the
//! token's own position in the rendered message, because a naive `contains(prefix)` loop
//! starting at one character would trip over ordinary prose (`gate.txt`, `deadbeef`, a
//! decimal constant) that shares single hex characters with the digest.

use serde_json::json;
use sha2::{Digest as _, Sha256};

fn digest_of(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The refusal bodies the `replace` guard actually renders, so each token sits at its real
/// position inside real diagnostic prose.
fn invalid_refusal(d: &str) -> String {
    format!(
        "replace blocked: expected_sha256 is invalid (it must be the full 64-character \
         lowercase hex sha256 of the complete current content; got 16 characters; a \
         16-character abbreviation is not usable); the current full digest of gate.txt is \
         {d}; no change made; use read_file to re-read the current content, confirm \
         old_string and new_string still express the intended edit, then retry with that \
         full digest; do not omit expected_sha256"
    )
}

fn stale_refusal(want: &str, d: &str) -> String {
    format!(
        "replace blocked: expected_sha256 {want} is stale for gate.txt; the current content \
         hashes to {d}; no change made; use read_file to re-read the current content, confirm \
         old_string still identifies the intended text and new_string is still the intended \
         replacement, then retry with that full digest; do not omit expected_sha256"
    )
}

/// The retained body length of a truncated rendering: the bytes before its truncation
/// marker. A tiny budget that cannot carry the marker keeps only a marker prefix, so the
/// retained body is empty.
fn retained_len(out: &str, marker_head: &str) -> usize {
    out.find(marker_head).unwrap_or(0)
}

/// The advertised field is either exactly the full digest or nothing: when the whole token
/// is absent, the retained body must stop at or before the token's own start offset, so not
/// even its first character is carried.
fn assert_field_whole_or_absent(out: &str, full: &str, at: usize, marker_head: &str, label: &str) {
    let token = &full[at..at + 64];
    if out.contains(token) {
        return;
    }
    if out.len() != full.len() && out.len() >= marker_head.len() {
        assert!(
            out.contains(marker_head),
            "{label}: the truncation marker head {marker_head:?} is absent from a truncated \
             rendering, so the marker format may have drifted; output: {out}"
        );
    }
    let body = retained_len(out, marker_head);
    assert!(
        body <= at,
        "{label}: {} leading character(s) of the digest reached the model: {out}",
        body - at
    );
}

/// Non-digest truncation behavior is untouched: prose without a digest token keeps its
/// exact bytes at every budget, including the marker-only tiny budgets.
#[test]
fn ordinary_prose_and_tiny_budgets_keep_their_exact_bytes() {
    let samples = [
        "a",
        "f",
        "deadbeef deadbeef",
        "0x1f",
        "version 1.2.3 build 77aa",
        "expected 4 occurrence(s) of x but found 7 in gate.txt",
    ];
    for s in samples {
        for max in 0..=(s.len() + 24) {
            let out = crate::redact::truncate_utf8(s.to_string(), max);
            assert!(out.len() <= max, "{s:?} at {max} grew: {out}");
            if s.len() > max {
                if max < crate::redact::TRUNCATION_MARKER.len() {
                    // Marker-only: the retained body is empty by definition.
                    assert_eq!(retained_len(&out, crate::redact::TRUNCATION_MARKER), 0);
                } else {
                    let keep = max - crate::redact::TRUNCATION_MARKER.len();
                    assert_eq!(
                        out,
                        format!(
                            "{}{}",
                            &s[..keep.min(s.len())],
                            crate::redact::TRUNCATION_MARKER
                        ),
                        "{s:?} at {max} was reshaped"
                    );
                }
            }
        }
    }
}

/// Every cut inside the token's 64 characters — including 1..7, below any guessed
/// "plausible prefix" threshold — at the post-scrub agent boundary, for both refusal
/// branches, with a second token in the stale branch.
#[test]
fn redact_cut_inside_the_digest_omits_it_whole_every_time() {
    let current = digest_of(b"issue243 digest boundary\n");
    let want = digest_of(b"previous bytes\n");
    for body in [invalid_refusal(&current), stale_refusal(&want, &current)] {
        for token in [&current, &want] {
            let Some(at) = body.find(token) else { continue };
            for cut in 1..=64usize {
                // Place the budget so the marker-reserving arithmetic cuts `cut` bytes
                // into the token.
                let max = at + cut - 1 + crate::redact::TRUNCATION_MARKER.len();
                let out = crate::redact::truncate_utf8(body.clone(), max);
                assert!(out.len() <= max, "cut {cut}: {out}");
                assert!(
                    out.contains(crate::redact::TRUNCATION_MARKER),
                    "cut {cut}: {out}"
                );
                assert_field_whole_or_absent(
                    &out,
                    &body,
                    at,
                    crate::redact::TRUNCATION_MARKER,
                    &format!("redact cut {cut}"),
                );
            }
        }
    }
}

/// The same exhaustive sweep through the real registered-tool dispatch, which renders the
/// refusal itself and then truncates under its own byte-counting marker arithmetic. The
/// binding budget here is the remaining turn output the agent passes in.
#[test]
fn registered_tool_cut_inside_the_digest_omits_it_whole_every_time() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("gate.txt"), b"registered tool body\n").unwrap();
    let cfg = crate::tools::ToolConfig {
        ws: crate::tools::WorkspaceCap::open(dir.path()).unwrap(),
        max_output_bytes: 16 * 1024,
        shell: crate::tools::ShellConfig {
            max_shell_output: 64 * 1024,
            max_shell_timeout: std::time::Duration::from_secs(60),
            allow_shell: false,
        },
    };
    let current = digest_of(b"registered tool body\n");
    let want = digest_of(b"previous bytes\n");
    let short16: &str = &current[..16];
    let short1: &str = &current[..1];
    for supplied in [short16, short1, want.as_str()] {
        let args = json!({
            "path": "gate.txt",
            "old_string": "registered",
            "new_string": "REGISTERED",
            "expected_sha256": supplied
        });
        let (_, full) = crate::tools::execute_tool_with_limit(
            dir.path(),
            "replace",
            args.clone(),
            &cfg,
            1 << 20,
        );
        let at = full
            .find(&current)
            .expect("the refusal names the current digest");
        for remaining in 1..=(full.len() + 1) {
            let (ok, out) = crate::tools::execute_tool_with_limit(
                dir.path(),
                "replace",
                args.clone(),
                &cfg,
                remaining,
            );
            assert!(!ok, "remaining {remaining} must refuse: {out}");
            assert!(out.len() <= remaining, "remaining {remaining}: {out}");
            assert_field_whole_or_absent(
                &out,
                &full,
                at,
                "...  [",
                &format!("remaining {remaining}"),
            );
            if let Some(want_at) = full.find(&want) {
                assert_field_whole_or_absent(
                    &out,
                    &full,
                    want_at,
                    "...  [",
                    &format!("want at remaining {remaining}"),
                );
            }
            assert_eq!(
                std::fs::read(dir.path().join("gate.txt")).unwrap(),
                b"registered tool body\n",
                "remaining {remaining} wrote"
            );
        }
    }
}

/// When the budget is adequate the full recovery guidance survives intact, so omitting a
/// fragmenting digest never costs the diagnostic its route.
#[test]
fn adequate_budget_keeps_the_whole_recovery_route() {
    let current = digest_of(b"issue243 digest boundary\n");
    let body = invalid_refusal(&current);
    let out = crate::redact::truncate_utf8(body.clone(), body.len() + 64);
    assert!(out.contains(&current), "{out}");
    assert!(out.contains("use read_file to re-read"), "{out}");
    assert!(out.contains("do not omit expected_sha256"), "{out}");
}
