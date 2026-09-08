use super::hash_gate::{stage, whole_file_digest, Fixture};
use crate::tools::digest_hex;
use crate::tools::execute_tool_with_limit;
use serde_json::json;

/// The registered-tool entry point with an explicit remaining-turn budget, mirroring
/// `agent`'s `execute_tool_with_limit` call: the same `min(tool cap, remaining)` limit
/// the loop applies. Every assertion here runs through this real dispatch path.
fn replace_with_budget(
    f: &Fixture,
    old: &str,
    new: &str,
    hash: Option<&str>,
    path: &str,
    remaining: usize,
    tool_cap: usize,
) -> (bool, String) {
    let ws = crate::tools::WorkspaceCap::open(&f.root).unwrap();
    let config = crate::tools::ToolConfig {
        ws,
        max_output_bytes: tool_cap,
        shell: crate::tools::ShellConfig {
            max_shell_output: 64 * 1024,
            max_shell_timeout: std::time::Duration::from_secs(60),
            allow_shell: false,
        },
    };
    let mut args = json!({"path": path, "old_string": old, "new_string": new});
    if let Some(h) = hash {
        args["expected_sha256"] = json!(h);
    }
    execute_tool_with_limit(&f.root, "replace", args, &config, remaining)
}

/// The advertised digest field is the whole 64-character value or nothing. A budget cut
/// that lands inside one produces a usable-looking abbreviation, which is exactly the
/// retry loop this diagnostic exists to end. The assertion is anchored on the field's own
/// offset in the uncut message rather than a naive `contains(prefix)` sweep starting at
/// one character, because ordinary prose (`gate.txt`, a decimal constant) shares single
/// hex characters with a digest and must stay byte-exact.
fn assert_no_partial_digest(uncut: &str, msg: &str, digest: &str, label: &str) {
    if msg.contains(digest) {
        return;
    }
    let Some(at) = uncut.find(digest) else {
        return;
    };
    // Only the retained body matters: the marker is the cut's own signal, so the body is
    // whatever precedes it. The field is either fully inside that body or fully outside it.
    if msg.len() < uncut.len() {
        assert!(
            msg.contains("...  ["),
            "{label}: a truncated message carries no truncation marker, so the marker format \
             may have drifted; message: {msg}"
        );
    }
    let body = msg.split("...  [").next().unwrap_or(msg);
    let carried = body.len().saturating_sub(at);
    if carried > 0 && carried < digest.len() {
        panic!(
            "{label}: a {carried}-character digest fragment reached the model without the full \
             digest in: {msg}"
        );
    }
}

/// Invalid syntax and stale refusals must never surface a digest that a later budget cut
/// turns into a usable-looking abbreviation. Tested across cap cuts that land inside the
/// digest position, at the marker boundary, and at plainly insufficient budgets.
#[test]
fn refusal_digest_survives_or_is_omitted_across_cap_cuts() {
    let fx = stage("gate body for cap cuts\n");
    let before = std::fs::read(&fx.path).unwrap();
    let current = whole_file_digest(&fx.path);
    let stale = digest_hex(b"other content entirely\n");
    for label in ["invalid", "stale"] {
        let supplied = match label {
            "invalid" => &current[..16],
            _ => stale.as_str(),
        };
        let (ok0, uncut) = replace_with_budget(
            &fx,
            "gate",
            "GATE",
            Some(supplied),
            "gate.txt",
            16 * 1024,
            16 * 1024,
        );
        assert!(!ok0, "{label}: {uncut}");
        for remaining in [
            24,
            64,
            96,
            128,
            160,
            192,
            224,
            256,
            320,
            384,
            448,
            512,
            640,
            768,
            1024,
            16 * 1024,
        ] {
            let (ok, msg) = replace_with_budget(
                &fx,
                "gate",
                "GATE",
                Some(supplied),
                "gate.txt",
                remaining,
                16 * 1024,
            );
            assert!(!ok, "{label} at {remaining} must refuse: {msg}");
            assert!(
                msg.len() <= remaining,
                "{label} at {remaining} exceeded the cap: {} bytes: {msg}",
                msg.len()
            );
            assert_no_partial_digest(&uncut, &msg, &current, &format!("{label} at {remaining}"));
            assert_no_partial_digest(&uncut, &msg, &stale, &format!("{label} at {remaining}"));
            assert_eq!(
                std::fs::read(&fx.path).unwrap(),
                before,
                "{label} at {remaining} must not write"
            );
        }
    }
}

/// A long path pushes the digest further back in the message, so cuts land in different
/// places; the invariant is unchanged.
#[test]
fn refusal_digest_invariant_with_long_paths() {
    let long_name = format!("{}.txt", "deep-name-".repeat(20));
    let fx = stage("content under a long name\n");
    std::fs::rename(&fx.path, fx.root.join(&long_name)).unwrap();
    let path = fx.root.join(&long_name);
    let before = std::fs::read(&path).unwrap();
    let current = whole_file_digest(&path);
    let (ok0, uncut) = replace_with_budget(
        &fx,
        "content",
        "CONTENT",
        Some(&current[..16]),
        &long_name,
        16 * 1024,
        16 * 1024,
    );
    assert!(!ok0, "{uncut}");
    for remaining in [
        40usize, 72, 120, 200, 300, 384, 400, 424, 456, 700, 900, 1200, 2048,
    ] {
        let (ok, msg) = replace_with_budget(
            &fx,
            "content",
            "CONTENT",
            Some(&current[..16]),
            &long_name,
            remaining,
            16 * 1024,
        );
        assert!(!ok, "long path at {remaining} must refuse: {msg}");
        assert!(msg.len() <= remaining, "{remaining}: {msg}");
        assert_no_partial_digest(&uncut, &msg, &current, &format!("long path at {remaining}"));
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

/// A configured tool cap below the remaining turn budget is the binding bound; the same
/// invariant must hold when it, not the turn, does the cutting.
#[test]
fn refusal_digest_invariant_when_tool_cap_binds() {
    let fx = stage("tool cap binding case\n");
    let before = std::fs::read(&fx.path).unwrap();
    let current = whole_file_digest(&fx.path);
    let (_, uncut) = replace_with_budget(
        &fx,
        "tool",
        "TOOL",
        Some(&current[..16]),
        "gate.txt",
        16 * 1024,
        16 * 1024,
    );
    for tool_cap in [48usize, 112, 176, 240, 304, 400, 512] {
        let (ok, msg) = replace_with_budget(
            &fx,
            "tool",
            "TOOL",
            Some(&current[..16]),
            "gate.txt",
            16 * 1024,
            tool_cap,
        );
        assert!(!ok, "tool cap {tool_cap} must refuse: {msg}");
        assert!(msg.len() <= tool_cap, "tool cap {tool_cap}: {msg}");
        assert_no_partial_digest(&uncut, &msg, &current, &format!("tool cap {tool_cap}"));
        assert_eq!(std::fs::read(&fx.path).unwrap(), before);
    }
}

/// When the budget is adequate the refusal keeps its full diagnostic value: the whole
/// digest, the read_file recheck, and the do-not-omit guidance all survive, and the
/// prescribed route actually recovers.
#[test]
fn adequate_budget_keeps_the_full_recovery_route() {
    let fx = stage("recovery route intact\n");
    let current = whole_file_digest(&fx.path);
    let stale = digest_hex(b"previous bytes\n");
    let (ok, msg) = replace_with_budget(
        &fx,
        "route",
        "ROUTE",
        Some(&stale),
        "gate.txt",
        16 * 1024,
        16 * 1024,
    );
    assert!(!ok);
    assert!(msg.contains(&current), "{msg}");
    assert!(msg.contains("use read_file to re-read"), "{msg}");
    assert!(msg.contains("confirm old_string"), "{msg}");
    assert!(msg.contains("do not omit expected_sha256"), "{msg}");
    // The route is genuinely usable: follow it and the guarded replace succeeds.
    let (ok, done) = replace_with_budget(
        &fx,
        "route",
        "ROUTE",
        Some(&current),
        "gate.txt",
        16 * 1024,
        16 * 1024,
    );
    assert!(ok, "{done}");
    assert_eq!(
        std::fs::read_to_string(&fx.path).unwrap(),
        "recovery ROUTE intact\n"
    );
}
