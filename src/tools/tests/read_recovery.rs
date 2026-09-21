//! Visible read bounds and digest re-fetch recipes (issue 125).
//!
//! A read clamped by the turn's remaining output budget must say so and must carry a
//! replayable recipe whose window stays under the digest admission floor, so the next
//! read is neither a smaller silent prefix nor a digest on the way back in.

use super::*;
use serde_json::json;
use std::time::Duration;

fn cfg_floor(root: &std::path::Path, floor: usize) -> ToolConfig {
    let ws = WorkspaceCap::open(root).unwrap();
    ToolConfig {
        ws,
        max_output_bytes: 16 * 1024,
        digest_size_floor: floor,
        shell: ShellConfig {
            max_shell_output: 64 * 1024,
            max_shell_timeout: Duration::from_secs(60),
            allow_shell: false,
        },
    }
}

/// The `re-fetch:` line of a truncated read, parsed back into its arguments object.
fn recipe_args(out: &str) -> (String, JsonValue) {
    let last = out
        .lines()
        .last()
        .unwrap_or_else(|| panic!("read result carries a last line: {out}"));
    assert!(
        last.starts_with("re-fetch: {"),
        "the recipe is the last line and is a JSON object: {out}"
    );
    (
        last.to_string(),
        serde_json::from_str(&last["re-fetch: ".len()..]).unwrap(),
    )
}

/// The rendered body between the header and the recipe lines, with one trailing cut
/// marker stripped: the byte length the recipe's offset must continue from, so no file
/// byte is skipped (issue 125).
fn rendered_body_len(out: &str) -> usize {
    let mut lines = out.lines();
    lines.next(); // the header
    lines.next_back(); // the recipe is guaranteed to be the last line
    let body = lines.collect::<Vec<_>>().join("\n");
    body.strip_suffix("...").unwrap_or(&body).len()
}

/// A read clamped by the output budget states the effective bound and its cause, and the
/// recipe's window continues exactly where the rendered body stopped while staying under
/// the configured digest floor.
#[test]
fn budget_clamped_read_states_the_bound_and_a_replayable_recipe() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("big.txt"), "x".repeat(2048)).unwrap();
    let config = cfg_floor(d.path(), 1024);
    let (ok, out) = execute_tool_with_limit(
        d.path(),
        "read_file",
        json!({"path": "big.txt", "offset": 400, "max_output_bytes": 4096}),
        &config,
        400,
    );
    assert!(ok, "{out}");
    assert!(
        out.contains(
            "[400..800 of 2048 bytes; window clamped to 400 bytes by output budget **truncated**]"
        ),
        "the header names the effective bound and the clamp cause: {out}"
    );
    assert!(out.len() <= 400, "the framed read stays inside the bound");
    let (line, args) = recipe_args(&out);
    let resume = 400 + rendered_body_len(&out);
    assert_eq!(
        line,
        format!("re-fetch: {{\"path\":\"big.txt\",\"offset\":{resume},\"max_output_bytes\":512}}")
    );
    assert_eq!(args["path"], json!("big.txt"));
    assert_eq!(
        args["offset"],
        json!(resume),
        "the recipe continues at the last rendered body byte"
    );
    assert!(args["offset"].as_u64().unwrap() <= 800);
    assert_eq!(args["max_output_bytes"], json!(512));
    assert!(
        args["max_output_bytes"].as_u64().unwrap() < 1024,
        "the recipe window stays under the digest floor"
    );
    assert_eq!(
        args["max_output_bytes"].as_u64().unwrap(),
        512,
        "the bulk seam's fixed 1024-byte threshold caps the verbatim window"
    );
}

/// A window that stopped at the caller's own bound (not a clamp) still carries a recipe
/// that continues exactly where the window stopped.
#[test]
fn eof_boundary_window_recipe_continues_where_the_window_stopped() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("big.txt"), "a".repeat(1024)).unwrap();
    let config = cfg_floor(d.path(), 1024);
    let (ok, out) = execute_tool_with_limit(
        d.path(),
        "read_file",
        json!({"path": "big.txt", "offset": 0, "limit": 200}),
        &config,
        16 * 1024,
    );
    assert!(ok, "{out}");
    assert!(
        out.contains("[0..200 of 1024 bytes; window 200 bytes at requested limit **truncated**]"),
        "{out}"
    );
    let (_, args) = recipe_args(&out);
    assert_eq!(
        args["offset"],
        json!(200),
        "the recipe resumes at the window end"
    );
    assert_eq!(args["max_output_bytes"], json!(512));

    // A window bounded by the caller's `max_output_bytes` (no explicit `limit`) is named
    // as such rather than blamed on the budget: the request bound the window, and the
    // framing's body abbreviation is what the cut marker and recipe communicate.
    let (ok, out) = execute_tool_with_limit(
        d.path(),
        "read_file",
        json!({"path": "big.txt", "offset": 0, "max_output_bytes": 200}),
        &config,
        16 * 1024,
    );
    assert!(ok, "{out}");
    assert!(
        out.contains("window 200 bytes at requested max_output_bytes"),
        "{out}"
    );
    let (_, args) = recipe_args(&out);
    assert_eq!(args["offset"], json!(rendered_body_len(&out)));
}

/// A raised floor widens the safe window the recipe offers, and a window that reaches the
/// end of the file carries no recipe at all.
#[test]
fn recipe_window_tracks_the_configured_floor_and_whole_reads_carry_none() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("big.txt"), "y".repeat(4096)).unwrap();
    let config = cfg_floor(d.path(), 4096);
    let (ok, out) = execute_tool_with_limit(
        d.path(),
        "read_file",
        json!({"path": "big.txt", "offset": 0, "limit": 100}),
        &config,
        16 * 1024,
    );
    assert!(ok, "{out}");
    let (_, args) = recipe_args(&out);
    assert_eq!(
        args["max_output_bytes"],
        json!(512),
        "the bulk seam's fixed threshold caps the window even at a raised floor"
    );

    std::fs::write(d.path().join("small.txt"), "whole file").unwrap();
    let (ok, out) = execute_tool_with_limit(
        d.path(),
        "read_file",
        json!({"path": "small.txt"}),
        &config,
        16 * 1024,
    );
    assert!(ok, "{out}");
    assert_eq!(out, "[0..10 of 10 bytes]\nwhole file");
    assert!(
        !out.contains("re-fetch:"),
        "a complete read needs no recipe: {out}"
    );
}

/// The recipe survives a bound too small for the body: it is reserved before the body
/// spends any of the budget.
#[test]
fn recipe_survives_a_bound_that_cuts_the_body() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("big.txt"), "z".repeat(2048)).unwrap();
    let config = cfg_floor(d.path(), 1024);
    for budget in [120usize, 400, 1024] {
        let (ok, out) = execute_tool_with_limit(
            d.path(),
            "read_file",
            json!({"path": "big.txt", "max_output_bytes": 4096}),
            &config,
            budget,
        );
        assert!(ok, "{out}");
        assert!(out.len() <= budget, "budget {budget}: {out}");
        assert!(
            out.lines().last().unwrap().starts_with("re-fetch: {"),
            "budget {budget} kept the recipe: {out}"
        );
    }
}

/// `search_file_content` discloses a budget clamp; no recipe is offered there because the
/// tool's own re-fetch is a narrower pattern, not a byte window.
#[test]
fn clamped_search_discloses_the_budget_bound() {
    let d = tempfile::tempdir().unwrap();
    let contents = (0..400)
        .map(|index| format!("needle-{index}\n"))
        .collect::<String>();
    std::fs::write(d.path().join("big.txt"), contents).unwrap();
    let config = cfg_floor(d.path(), 1024);
    let (ok, out) = execute_tool_with_limit(
        d.path(),
        "search_file_content",
        json!({"pattern": "needle", "max_output_bytes": 4096}),
        &config,
        300,
    );
    assert!(ok, "{out}");
    assert!(out.len() <= 300, "{out}");
    assert!(
        out.contains(" [output clamped to 300 bytes by output budget]"),
        "the clamp is disclosed: {out}"
    );
}

/// The cut marker is reserved once (issue 125): at `cap = fixed+1` and `fixed+2` the
/// framed read still fits the bound exactly and never underflows the body budget.
#[test]
fn cut_marker_is_reserved_once_at_the_boundary_caps() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("big.txt"), "z".repeat(2048)).unwrap();
    let config = cfg_floor(d.path(), 1024);
    let (ok, out) = execute_tool_with_limit(
        d.path(),
        "read_file",
        json!({"path": "big.txt", "max_output_bytes": 4096}),
        &config,
        1024,
    );
    assert!(ok, "{out}");
    let header = out.lines().next().unwrap();
    let recipe = out.lines().last().unwrap();
    assert!(recipe.starts_with("re-fetch: {"), "{out}");
    let fixed = header.len() + 3 + recipe.len() + 2;
    for budget in [fixed + 1, fixed + 2] {
        let (ok, out) = execute_tool_with_limit(
            d.path(),
            "read_file",
            json!({"path": "big.txt", "max_output_bytes": 4096}),
            &config,
            budget,
        );
        assert!(ok, "budget {budget}: {out}");
        assert!(
            out.len() <= budget,
            "budget {budget} (fixed {fixed}) held: {}",
            out.len()
        );
        // Cross-budget recipe byte identity is not an invariant: the rendered body (and
        // so the recipe's resume offset) legitimately differs per budget. The invariants
        // are the recipe's presence, parseability, path, and A1 continuity.
        let last = out.lines().next_back().unwrap();
        assert!(
            last.starts_with("re-fetch: {"),
            "budget {budget} kept a parseable recipe: {out}"
        );
        let args: serde_json::Value = serde_json::from_str(&last["re-fetch: ".len()..]).unwrap();
        assert_eq!(args["path"], json!("big.txt"));
        assert_eq!(
            args["offset"],
            json!(rendered_body_len(&out)),
            "budget {budget}: the recipe continues at the last rendered body byte"
        );
    }
}

/// A per-call bound too small to carry a header byte, the newline, and the whole recipe
/// is refused before framing instead of returning an unparseable cut recipe (issue 125).
#[test]
fn a_bound_too_small_for_the_frame_is_a_typed_error() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("big.txt"), "z".repeat(2048)).unwrap();
    let config = cfg_floor(d.path(), 1024);
    // The refusal message (~82 bytes) cannot survive the crate's 8-byte truncation
    // verbatim (a long value at cap 8 truncates to its marker prefix), so the tiny-bound
    // case asserts the refusal plus that prefix, and a cap-96 call carries the exact
    // message end to end: the budget also caps the read itself (the window's end digit
    // count moves with it), and the smallest real frame — short header + newline + cut
    // marker + newline + the 63-byte recipe — sits in the low hundreds, so 96 is safely
    // below it while the message fits the cap (issue 125 cycle 2).
    let (ok, out) = execute_tool_with_limit(
        d.path(),
        "read_file",
        json!({"path": "big.txt", "max_output_bytes": 4096}),
        &config,
        8,
    );
    assert!(!ok, "the tiny bound is refused: {out}");
    assert!(
        out.starts_with("...  [tr"),
        "the 8-byte cap truncates to the marker prefix: {out:?}"
    );
    let (ok, out) = execute_tool_with_limit(
        d.path(),
        "read_file",
        json!({"path": "big.txt", "max_output_bytes": 4096}),
        &config,
        96,
    );
    assert!(
        !ok,
        "a bound below the smallest real frame is refused: {out}"
    );
    assert_eq!(
        out,
        "output budget 96 bytes is too small to render a read window and its re-fetch recipe"
    );
}

/// The model-visible `read_file` string (window header plus body) is at most
/// `max_output` bytes total.
#[test]
fn read_file_total_is_bounded_including_frame() {
    let d = tempfile::tempdir().unwrap();
    let big = "y".repeat(MAX_FILE_BYTES);
    std::fs::write(d.path().join("big.txt"), &big).unwrap();
    let ws = WorkspaceCap::open(d.path()).unwrap();
    let c = ToolConfig {
        ws,
        max_output_bytes: 8192,
        digest_size_floor: crate::context_ingress::filter::DEFAULT_DIGEST_SIZE_FLOOR,
        shell: ShellConfig {
            max_shell_output: 64 * 1024,
            max_shell_timeout: Duration::from_secs(30),
            allow_shell: false,
        },
    };
    let (ok, body) = execute_tool(d.path(), "read_file", json!({"path": "big.txt"}), &c);
    assert!(ok, "{body}");
    assert!(
        body.len() <= 8192,
        "read_file total must be <= max_output: {}",
        body.len()
    );
}

#[test]
fn recipe_escapes_a_path_containing_a_quote_and_backslash() {
    let d = tempfile::tempdir().unwrap();
    let name = "we\"ird\\name.txt";
    std::fs::write(d.path().join(name), "y".repeat(4096)).unwrap();
    let config = cfg_floor(d.path(), 1024);
    let (ok, out) = execute_tool_with_limit(
        d.path(),
        "read_file",
        json!({"path": name, "offset": 0}),
        &config,
        400,
    );
    assert!(ok, "{out}");
    let (line, args) = recipe_args(&out);
    assert_eq!(
        args["path"].as_str().unwrap(),
        name,
        "the recipe must parse and round-trip the exact name: {line}"
    );

    // Control characters are escaped by the JSON encoder, so the recipe stays one line.
    let name = "tab\tand[r- nl.txt";
    std::fs::write(d.path().join(name), "y".repeat(4096)).unwrap();
    let (ok, out) = execute_tool_with_limit(
        d.path(),
        "read_file",
        json!({"path": name, "offset": 0}),
        &config,
        400,
    );
    assert!(ok, "{out}");
    let (line, args) = recipe_args(&out);
    assert_eq!(
        line.lines().count(),
        1,
        "the recipe is a single line: {line}"
    );
    assert_eq!(
        args["path"].as_str().unwrap(),
        name,
        "the exact name round-trips through the escaped recipe: {line}"
    );
}
