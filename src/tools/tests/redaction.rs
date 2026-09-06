//! Issue 148 coverage: ordinary punctuation in tool output must survive the redaction
//! layer byte-identically, and the read/write edit flow must not corrupt source.

use super::*;
use serde_json::json;

/// Issue 148: the tool-result scrubber used to rewrite every bare `?` and `#`
/// into a `[r-----` style placeholder, so an agent that read a file and wrote what it
/// saw back corrupted source on disk. The agent layer runs `scrub_secrets` over raw
/// tool output before it reaches the model (and before truncation), so this proves the
/// same bytes that come back from `read_file` survive that layer and re-write
/// byte-identically through `write_file`.
#[test]
fn file_edit_round_trips_ordinary_punctuation_byte_identically() {
    let d = tempfile::tempdir().unwrap();
    let corpus = concat!(
        "#!/bin/sh\n",
        "#[test]\n",
        "#[cfg_attr(miri, ignore)]\n",
        "path = C:\\Users\\me\\src\\lib.rs\n",
        "let v = s.split('\\n').find(|l| l.starts_with(\"# \"))?;\n",
        "grep -n #TODO\" src\n",
        "whoami? root\n",
    );
    let (wrote, wmsg) = run(
        d.path(),
        "write_file",
        json!({"path": "round.rs", "content": corpus}),
    );
    assert!(wrote, "write_file failed: {wmsg}");
    // `read_file` frames the bytes with a `[0..N of N bytes]` window header; the body
    // under that header must be the disk bytes verbatim.
    assert_eq!(
        std::fs::read(d.path().join("round.rs")).unwrap(),
        corpus.as_bytes(),
        "write_file must store the corpus byte-identically"
    );
    let (ok, body) = run(d.path(), "read_file", json!({"path": "round.rs"}));
    assert!(ok, "read_file failed: {body}");
    let read_back = body.split_once('\n').map(|(_, rest)| rest).unwrap_or("");
    assert_eq!(
        read_back, corpus,
        "read_file must return disk bytes verbatim"
    );
    // The same text the model would see after the redaction layer must round-trip too.
    let seen_by_model = crate::redact::scrub_secrets(read_back, &[]);
    assert_eq!(
        seen_by_model, corpus,
        "redaction mangled ordinary punctuation"
    );
    let (ok, msg) = run(
        d.path(),
        "write_file",
        json!({"path": "round2.rs", "content": seen_by_model}),
    );
    assert!(ok, "rewrite failed: {msg}");
    assert_eq!(
        std::fs::read(d.path().join("round2.rs")).unwrap(),
        corpus.as_bytes(),
        "the edit flow corrupted the file bytes"
    );
}

/// Issue 129: a Rust source file whose attribute lines are ordinary (no `://` behind
/// them) must come back from `read_file` with those bytes intact, both at the tool
/// boundary and after the redaction layer the agent applies before the model sees the
/// text. A `#[derive(...)]`, `#[test]`, and `#[cfg(...)]` line that shares a document
/// with an ordinary prose sentence must never be rewritten into a `[r-----` form.
#[test]
fn read_file_returns_rust_attribute_lines_byte_identically() {
    let d = tempfile::tempdir().unwrap();
    let disk = concat!(
        "//! Module docs.\n",
        "#![allow(dead_code)]\n",
        "\n",
        "#[derive(Debug, Clone, PartialEq, Eq)]\n",
        "pub struct Sample {\n",
        "    pub id: u64,\n",
        "}\n",
        "\n",
        "#[cfg(feature = \"extra\")]\n",
        "mod extra {}\n",
        "\n",
        "#[cfg(test)]\n",
        "mod tests {\n",
        "    #[test]\n",
        "    fn sample_is_debug() {\n",
        "        let s = Sample { id: 1 };\n",
        "        let _ = format!(\"{s:?}\");\n",
        "    }\n",
        "}\n",
    );
    let (wrote, wmsg) = run(
        d.path(),
        "write_file",
        json!({"path": "attrs.rs", "content": disk}),
    );
    assert!(wrote, "write_file failed: {wmsg}");
    assert_eq!(
        std::fs::read(d.path().join("attrs.rs")).unwrap(),
        disk.as_bytes(),
        "write_file must store the attribute bytes verbatim"
    );
    let (ok, body) = run(d.path(), "read_file", json!({"path": "attrs.rs"}));
    assert!(ok, "read_file failed: {body}");
    let tool_body = body.split_once('\n').map(|(_, rest)| rest).unwrap_or("");
    assert_eq!(
        tool_body, disk,
        "read_file must return the attribute lines verbatim"
    );
    let seen_by_model = crate::redact::scrub_secrets(tool_body, &[]);
    assert_eq!(
        seen_by_model, disk,
        "the redaction layer mangled the Rust attribute lines"
    );
    for line in [
        "#![allow(dead_code)]\n",
        "#[derive(Debug, Clone, PartialEq, Eq)]\n",
        "#[cfg(feature = \"extra\")]\n",
        "#[cfg(test)]\n",
        "    #[test]\n",
    ] {
        assert!(
            seen_by_model.contains(line),
            "attribute line `{line}` did not survive: {seen_by_model}"
        );
    }
}
