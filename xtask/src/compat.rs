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

fn strip_comments(line: &str) -> &str {
    line.split("//").next().unwrap_or("")
}

/// Scan one file. Every code line is judged on its own text; nothing about a
/// neighboring line, comment or otherwise, can hide a banned identifier or a
/// format-fallback shape.
fn scan_file(path: &Path, rel: &str, findings: &mut Vec<String>) -> Result<(), String> {
    let content =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let lines: Vec<&str> = content.lines().collect();
    for (index, raw) in lines.iter().enumerate() {
        if raw.trim_start().starts_with("//") {
            continue;
        }
        let code = strip_comments(raw).trim();
        for marker in BANNED_MARKERS {
            if code.contains(marker) {
                findings.push(format!("{}:{}: banned marker '{}'", rel, index + 1, marker));
                break;
            }
        }
        let window: Vec<&str> = lines.iter().skip(index + 1).take(3).copied().collect();
        let is_if_guard = code.contains("if") && code.contains("is_err");
        let body_exits = window.iter().any(|l| {
            let w = strip_comments(l);
            w.contains("return") || w.contains("continue")
        });
        let fallback_arm = code.contains("Err(_) =>") || is_if_guard;
        let guard_exits =
            code.contains("return") || code.contains("continue") || (is_if_guard && body_exits);
        if fallback_arm
            && !guard_exits
            && window
                .iter()
                .any(|l| strip_comments(l).contains("serde_json::from_"))
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
        fs::remove_dir_all(&root).unwrap();
        assert!(result.is_ok(), "clean sources need no authorization file");
        assert!(!root.join("compat-ledger.md").exists());
        assert!(!root.join("xtask").join("compat-allowlist").exists());
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
    /// fabricated authorization file changes that outcome.
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
            fs::remove_dir_all(&root).unwrap();
            assert!(error.contains("compat gate failed"), "error: {error}");
        }
    }
}
