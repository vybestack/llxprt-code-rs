//! Gate: production sources stay one-directional. No legacy readers,
//! migrations, or compatibility shims (issues 74 and 234).
//!
//! Nothing here can grant an exception: the gate has no allowlist, no ledger,
//! and no marker suppression. Any line that names compatibility machinery is a
//! finding, and a comment that merely looks like a retired authorization marker
//! suppresses neither that line nor its neighbors.

use std::fs;
use std::path::{Path, PathBuf};

/// Identifiers that name compatibility machinery rather than behavior.
const BANNED_MARKERS: [&str; 10] = [
    "legacy",
    "migrate_",
    "back_compat",
    "backwards_compat",
    "compat_",
    "fallback_",
    "deprecated",
    "_v1",
    "_v2",
    "compatibility",
];

pub fn run(root: &Path) -> Result<(), String> {
    let (count, findings) = scan_findings(root)?;
    if findings.is_empty() {
        println!("compat gate passed: {count} production files scanned, 0 exceptions");
        Ok(())
    } else {
        for finding in &findings {
            eprintln!("compat gate: {finding}");
        }
        Err(format!("compat gate failed: {} findings", findings.len()))
    }
}

/// Scan every production source into `findings`, returning the file count and
/// the sorted findings; used by `run` and by the tests that exercise the
/// filesystem scan directly.
fn scan_findings(root: &Path) -> Result<(usize, Vec<String>), String> {
    let files = production_sources(root)?;
    let mut findings = Vec::new();
    for file in &files {
        let rel = file
            .strip_prefix(root)
            .map_err(|_| format!("path {} escapes root", file.display()))?
            .to_string_lossy()
            .into_owned();
        scan_file(file, &rel, &mut findings)?;
    }
    findings.sort();
    Ok((files.len(), findings))
}

/// All `src/**/*.rs` production files plus their in-file test modules.
fn production_sources(root: &Path) -> Result<Vec<PathBuf>, String> {
    let src = root.join("src");
    let mut files = Vec::new();
    collect_rs_files(&src, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_rs_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|error| format!("read {}: {error}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("read {} entry: {error}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, files)?;
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
    Ok(())
}

/// Where the stripper currently is in the file it is walking.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LexState {
    /// Ordinary code; a comment can only start here.
    Code,
    /// Inside an ordinary string or byte string literal.
    Str,
    /// Inside a raw string whose opening delimiter used `hashes` `#` signs.
    RawStr { hashes: usize },
    /// Inside a char or byte char literal.
    Char,
    /// Inside a block comment nested `depth` levels deep.
    Block { depth: usize },
}

/// Strip Rust comments from `content`, returning exactly one code-only line per
/// input line.
///
/// This is a lexer rather than a byte search: `//` inside a string, a raw
/// string, or a char literal is never mistaken for a comment start, block
/// comments nest and span lines, and raw strings close only on the `#` count of
/// their own opening delimiter. Literal contents are copied through verbatim,
/// so a banned identifier sitting inside a string or char literal stays in the
/// scanned text; only genuine comment text is dropped. Line numbers line up
/// one-for-one with the file on disk because a comment-only line becomes an
/// empty line instead of vanishing.
fn strip_comments(content: &str) -> Vec<String> {
    let mut stripped = Vec::new();
    let mut line = String::new();
    let mut state = LexState::Code;
    let mut chars = content.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\n' {
            stripped.push(std::mem::take(&mut line));
            continue;
        }
        match state {
            LexState::Code => match ch {
                '"' => {
                    line.push(ch);
                    state = LexState::Str;
                }
                '\'' => {
                    // A quote followed by an identifier that is not immediately
                    // closed is a lifetime rather than a character literal.
                    let mut name = String::new();
                    while chars
                        .peek()
                        .is_some_and(|c| c.is_alphanumeric() || *c == '_')
                    {
                        name.push(chars.next().unwrap());
                    }
                    line.push('\'');
                    line.push_str(&name);
                    if name.is_empty() || chars.peek() == Some(&'\'') {
                        state = LexState::Char;
                    }
                }
                'r' => {
                    let mut hashes = 0;
                    while chars.peek() == Some(&'#') {
                        chars.next();
                        hashes += 1;
                    }
                    line.push('r');
                    for _ in 0..hashes {
                        line.push('#');
                    }
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        line.push('"');
                        state = LexState::RawStr { hashes };
                    }
                }
                '/' => match chars.peek() {
                    // Line comment: drain to the end of this line.
                    Some(&'/') => {
                        while chars.peek().is_some_and(|c| *c != '\n') {
                            chars.next();
                        }
                    }
                    Some(&'*') => {
                        chars.next();
                        state = LexState::Block { depth: 1 };
                    }
                    _ => line.push(ch),
                },
                _ => line.push(ch),
            },
            LexState::Str => {
                line.push(ch);
                match ch {
                    '\\' => match chars.next() {
                        // An escaped newline continues the literal on the next
                        // line, so it still costs one output line.
                        Some('\n') => stripped.push(std::mem::take(&mut line)),
                        Some(escaped) => line.push(escaped),
                        None => {}
                    },
                    '"' => state = LexState::Code,
                    _ => {}
                }
            }
            LexState::RawStr { hashes } => {
                if ch == '"' {
                    let mut matched = 0;
                    while matched < hashes && chars.peek() == Some(&'#') {
                        chars.next();
                        matched += 1;
                    }
                    line.push('"');
                    for _ in 0..matched {
                        line.push('#');
                    }
                    if matched == hashes {
                        state = LexState::Code;
                    }
                } else {
                    line.push(ch);
                }
            }
            LexState::Char => {
                line.push(ch);
                match ch {
                    '\\' => {
                        if let Some(escaped) = chars.next() {
                            line.push(escaped);
                        }
                    }
                    '\'' => state = LexState::Code,
                    _ => {}
                }
            }
            LexState::Block { depth } => match ch {
                '/' if chars.peek() == Some(&'*') => {
                    chars.next();
                    state = LexState::Block { depth: depth + 1 };
                }
                '*' if chars.peek() == Some(&'/') => {
                    chars.next();
                    state = if depth == 1 {
                        LexState::Code
                    } else {
                        LexState::Block { depth: depth - 1 }
                    };
                }
                _ => {}
            },
        }
    }
    stripped.push(line);
    stripped
}

/// Scan one file. Every code line is judged on its own text; nothing about a
/// neighboring line, comment or otherwise, can hide a banned identifier or a
/// format-fallback shape. Comments are stripped first, so a comment-only line
/// contributes nothing while the literals around it keep their contents.
fn scan_file(path: &Path, rel: &str, findings: &mut Vec<String>) -> Result<(), String> {
    let content =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let code_lines = strip_comments(&content);
    for (index, code) in code_lines.iter().map(|line| line.trim()).enumerate() {
        for marker in BANNED_MARKERS {
            if code.contains(marker) {
                findings.push(format!("{}:{}: banned marker '{}'", rel, index + 1, marker));
                break;
            }
        }
        let window: Vec<&str> = code_lines
            .iter()
            .skip(index + 1)
            .take(3)
            .map(|line| line.trim())
            .collect();
        let is_if_guard = code.contains("if") && code.contains("is_err");
        let body_exits = window
            .iter()
            .any(|line| line.contains("return") || line.contains("continue"));
        let fallback_arm = code.contains("Err(_) =>") || is_if_guard;
        let guard_exits =
            code.contains("return") || code.contains("continue") || (is_if_guard && body_exits);
        if fallback_arm
            && !guard_exits
            && window.iter().any(|line| line.contains("serde_json::from_"))
        {
            findings.push(format!(
                "{}:{}: format-fallback shape (second deserialization inside an Err arm)",
                rel,
                index + 1
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A throwaway repository root holding only the sources under test.
    fn temp_root(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("llxprt-compat-gate-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(path.join("src")).unwrap();
        path
    }

    fn write(root: &Path, rel: &str, contents: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn findings(root: &Path) -> Vec<String> {
        super::scan_findings(root).unwrap().1
    }

    #[test]
    fn flags_banned_legacy_reader() {
        let root = temp_root("legacy-marker");
        write(
            &root,
            "src/x.rs",
            "fn read_legacy_state(dir: &openat::Dir) {}\n",
        );
        let found = findings(&root);
        fs::remove_dir_all(&root).unwrap();
        assert_eq!(found.len(), 1, "findings: {found:?}");
        assert!(found[0].contains("banned marker 'legacy'"));
    }

    #[test]
    fn retired_marker_comment_does_not_suppress_its_own_line() {
        for (name, source) in [
            (
                "cited",
                "    read_legacy_state(dir)  // compat-allow([r): documented exception\n",
            ),
            (
                "bare",
                "    read_legacy_state(dir)  // compat-allow: old form\n",
            ),
            (
                "ledger",
                "    read_legacy_state(dir)  // compat-allow(234): ledgered\n",
            ),
            (
                "prose",
                "    read_legacy_state(dir)  // compat-allow #234 approved elsewhere\n",
            ),
        ] {
            let root = temp_root(&format!("own-line-{name}"));
            write(&root, "src/x.rs", source);
            let found = findings(&root);
            fs::remove_dir_all(&root).unwrap();
            assert_eq!(
                found.len(),
                1,
                "{name}: the marker comment must not hide its own line: {found:?}"
            );
            assert!(found[0].contains("banned marker 'legacy'"));
        }
    }

    #[test]
    fn retired_marker_comment_does_not_suppress_neighbor_lines() {
        for name in ["preceding", "following"] {
            let root = temp_root(&format!("neighbor-{name}"));
            let source = if name == "preceding" {
                "    // compat-allow(234): retired marker\n    read_legacy_state(dir)\n"
            } else {
                "    read_legacy_state(dir)\n    // compat-allow(234): retired marker\n"
            };
            write(&root, "src/x.rs", source);
            let found = findings(&root);
            fs::remove_dir_all(&root).unwrap();
            assert_eq!(
                found.len(),
                1,
                "{name}: the marker comment must not hide adjacent code: {found:?}"
            );
            assert!(found[0].contains("banned marker 'legacy'"));
        }
    }

    #[test]
    fn harmless_sources_and_plain_comments_pass() {
        let root = temp_root("clean");
        write(
            &root,
            "src/x.rs",
            concat!(
                "// An ordinary comment never needs authority and never hides code.\n",
                "pub fn add(left: u32, right: u32) -> u32 {\n",
                "    left + right\n",
                "}\n",
                "\n",
                "let note = \"serde_json::from_slice is a supported parse\";\n"
            ),
        );
        write(&root, "src/nested/mod.rs", "pub const ANSWER: u8 = 42;\n");
        let found = findings(&root);
        fs::remove_dir_all(&root).unwrap();
        assert!(found.is_empty(), "findings: {found:?}");
    }

    #[test]
    fn flags_format_fallback_shape() {
        let root = temp_root("fallback-shape");
        write(
            &root,
            "src/x.rs",
            "match serde_json::from_slice::<StateSlot>(&bytes) {\n    Ok(slot) => slot,\n    Err(_) => {\n        match serde_json::from_slice::<SessionState>(&bytes) {\n            Ok(state) => state,\n            Err(_) => return Err(Corrupt),\n        }\n    }\n}\n",
        );
        let found = findings(&root);
        fs::remove_dir_all(&root).unwrap();
        assert!(
            found.iter().any(|f| f.contains("format-fallback shape")),
            "findings: {found:?}"
        );
    }

    #[test]
    fn skips_error_guards_that_exit_before_parsing() {
        let root = temp_root("guard-exit");
        write(
            &root,
            "src/x.rs",
            concat!(
                "if reader.read_exact(&mut body).is_err() {\n",
                "    return;\n",
                "}\n",
                "let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);\n"
            ),
        );
        let found = findings(&root);
        fs::remove_dir_all(&root).unwrap();
        assert!(
            !found.iter().any(|f| f.contains("format-fallback shape")),
            "guard-exit followed by an unrelated parse must not flag: {found:?}"
        );
    }

    #[test]
    fn run_passes_without_any_authorization_files() {
        let root = temp_root("run-clean");
        write(
            &root,
            "src/x.rs",
            "pub fn add(left: u32, right: u32) -> u32 {\n    left + right\n}\n",
        );
        let result = run(&root);
        assert!(result.is_ok(), "clean sources need no authorization file");
        assert!(!root.join("compat-ledger.md").exists());
        assert!(!root.join("xtask").join("compat-allowlist").exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn dropped_authorization_files_cannot_grant_permission() {
        let root = temp_root("run-authorized-but-banned");
        write(
            &root,
            "src/x.rs",
            "    read_legacy_state(dir)  // compat-allow(234): retired marker\n",
        );
        write(
            &root,
            "compat-ledger.md",
            "| Site | Reason |\n| --- | --- |\n| src/x.rs:1 | retired authorization |\n",
        );
        fs::create_dir_all(root.join("xtask")).unwrap();
        fs::write(root.join("xtask/compat-allowlist"), "234\n").unwrap();
        let error = run(&root).expect_err("no file can authorize a banned identifier");
        fs::remove_dir_all(&root).unwrap();
        assert!(error.contains("compat gate failed"), "error: {error}");
        assert!(error.contains("1 findings"), "error: {error}");
    }

    /// The rustc-attribute spelling the flow grader deliberately does not model
    /// is pinned here unobfuscated so it can never be reintroduced under a
    /// scrambled name: writing a realistic grader source line containing it
    /// into an ordinary scanned root must fail the public `run` scan, and no
    /// fabricated authorization file changes that outcome. The finding lines
    /// `run` prints to stderr are asserted directly, not just the summary.
    #[test]
    fn unmodeled_attribute_name_still_fails_the_public_scan() {
        for name in ["plain", "with_fabricated_authorization_files"] {
            let root = temp_root(&format!("unmodeled-attr-{name}"));
            write(
                &root,
                "src/grade/flow/collector.rs",
                "impl<'ast> syn::visit::Visit<'ast> for UnsupportedSyntaxCollector {\n    fn visit_attribute(&mut self, attr: &'ast syn::Attribute) {\n        if attr.path().is_ident(\"inline\") || attr.path().is_ident(\"deprecated\") {\n            self.unsupported.insert(attr.span());\n        }\n    }\n}\n",
            );
            if name == "with_fabricated_authorization_files" {
                write(
                    &root,
                    "compat-ledger.md",
                    "| Site | Reason |\n| --- | --- |\n| src/grade/flow/collector.rs:3 | retired authorization |\n",
                );
                fs::create_dir_all(root.join("xtask")).unwrap();
                fs::write(root.join("xtask/compat-allowlist"), "250\n").unwrap();
            }
            let error = run(&root).expect_err("the unmodeled attribute name must be a finding");
            let printed = findings(&root);
            fs::remove_dir_all(&root).unwrap();
            assert!(error.contains("compat gate failed"), "error: {error}");
            assert!(error.contains("1 findings"), "error: {error}");
            let report = format!("error: {error}\nfindings: {printed:?}");
            assert_eq!(
                printed.len(),
                1,
                "the scan must report exactly one finding: {report}"
            );
            assert!(
                printed[0].contains("banned marker 'deprecated'"),
                "the scan output must name the banned marker: {report}"
            );
            assert!(
                printed[0].contains("src/grade/flow/collector.rs"),
                "the scan output must name the offending file: {report}"
            );
            assert!(
                printed[0].contains("src/grade/flow/collector.rs:3: banned marker 'deprecated'"),
                "the finding must pin the site and the banned marker the fixture plants: {report}"
            );
        }
    }

    /// The stripper is a lexer, not a byte search: a `//` inside a string
    /// literal is data, so the code after it on the same line stays scanned.
    #[test]
    fn url_in_a_string_literal_does_not_start_a_comment() {
        let root = temp_root("url-string");
        write(
            &root,
            "src/x.rs",
            "let url = \"https://host/path\"; read_legacy_state(dir);\n",
        );
        let found = findings(&root);
        fs::remove_dir_all(&root).unwrap();
        assert_eq!(
            found.len(),
            1,
            "a URL in a string literal must not hide the code after it: {found:?}"
        );
        assert!(found[0].contains("banned marker 'legacy'"));
    }

    /// A comment is a comment: a banned identifier written only inside a
    /// genuine line comment is not part of the program.
    #[test]
    fn banned_marker_inside_a_line_comment_is_not_a_finding() {
        let root = temp_root("line-comment");
        write(
            &root,
            "src/x.rs",
            "// read_legacy_state(dir) is gone for good\n",
        );
        let found = findings(&root);
        fs::remove_dir_all(&root).unwrap();
        assert!(found.is_empty(), "findings: {found:?}");
    }

    /// Block comments nest and span lines, and code after the closing
    /// delimiter on the very same line is still scanned.
    #[test]
    fn banned_marker_inside_a_block_comment_is_not_a_finding() {
        let root = temp_root("block-comment");
        write(
            &root,
            "src/x.rs",
            concat!(
                "/* outer read_legacy_state(dir)\n",
                "   /* nested read_migrate_state(dir) */ still a comment\n",
                "   still a comment */ read_legacy_state(dir)\n"
            ),
        );
        let found = findings(&root);
        fs::remove_dir_all(&root).unwrap();
        assert_eq!(
            found.len(),
            1,
            "only the code after the closing delimiter may be a finding: {found:?}"
        );
        assert!(found[0].contains("src/x.rs:3"), "finding: {}", found[0]);
        assert!(found[0].contains("banned marker 'legacy'"));
    }

    /// Raw strings close on their own `#` count, so `//` inside one is data.
    #[test]
    fn raw_string_containing_slashes_is_scanned_as_code() {
        let root = temp_root("raw-string");
        write(
            &root,
            "src/x.rs",
            "let s = r#\"https://x\"#; read_legacy_state(d);\n",
        );
        let found = findings(&root);
        fs::remove_dir_all(&root).unwrap();
        assert_eq!(found.len(), 1, "findings: {found:?}");
        assert!(found[0].contains("banned marker 'legacy'"));
    }

    /// A slash character literal is data, not the start of a line comment.
    #[test]
    fn slash_char_literal_is_scanned_as_code() {
        let root = temp_root("char-literal");
        write(&root, "src/x.rs", "let c = '/'; read_legacy_state(d);\n");
        let found = findings(&root);
        fs::remove_dir_all(&root).unwrap();
        assert_eq!(found.len(), 1, "findings: {found:?}");
        assert!(found[0].contains("banned marker 'legacy'"));
    }

    /// Scanner strength: stripping comments must never mask a banned
    /// identifier that lives inside a string literal.
    #[test]
    fn banned_marker_inside_a_string_literal_is_still_a_finding() {
        let root = temp_root("string-literal");
        write(
            &root,
            "src/x.rs",
            "let reason = \"the legacy reader is gone\"; // and stays gone\n",
        );
        let found = findings(&root);
        fs::remove_dir_all(&root).unwrap();
        assert_eq!(found.len(), 1, "findings: {found:?}");
        assert!(found[0].contains("banned marker 'legacy'"));
    }

    /// The format-fallback window is judged on comment-stripped lines, so a
    /// comment-only line contributes nothing while the surrounding code still
    /// forms the shape.
    #[test]
    fn format_fallback_window_uses_comment_stripped_lines() {
        let root = temp_root("fallback-window");
        write(
            &root,
            "src/x.rs",
            concat!(
                "match serde_json::from_slice::<StateSlot>(&bytes) {\n",
                "    Ok(slot) => slot,\n",
                "    // the old reader is gone\n",
                "    Err(_) => {\n",
                "        match serde_json::from_slice::<SessionState>(&bytes) {\n",
                "            Ok(state) => state,\n",
                "            Err(_) => return Err(Corrupt),\n",
                "        }\n",
                "    }\n",
                "}\n"
            ),
        );
        let found = findings(&root);
        fs::remove_dir_all(&root).unwrap();
        assert!(
            found
                .iter()
                .any(|finding| finding.contains("format-fallback shape")),
            "findings: {found:?}"
        );
    }
}
