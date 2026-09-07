//! Gate: production sources stay one-directional. No legacy readers,
//! migrations, or compatibility shims (issue 74).

use std::fs;
use std::path::{Path, PathBuf};

/// Marker a source line may carry to allow one flagged finding with a reason.
const ALLOW_MARKER: &str = "// compat-allow:";

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
    let mut findings = Vec::new();
    for file in production_sources(root)? {
        scan_file(&file, &mut findings)?;
    }
    findings.sort();
    if findings.is_empty() {
        println!(
            "compat gate passed: {} production files scanned",
            production_sources(root)?.len()
        );
        Ok(())
    } else {
        for finding in &findings {
            eprintln!("compat gate: {finding}");
        }
        Err(format!("compat gate failed: {} findings", findings.len()))
    }
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

fn scan_file(path: &Path, findings: &mut Vec<String>) -> Result<(), String> {
    let content =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let lines: Vec<&str> = content.lines().collect();
    for (index, raw) in lines.iter().enumerate() {
        let trimmed = raw.trim();
        let annot = |i: usize| {
            let l: Vec<&str> = content.lines().collect();
            i < l.len() && l[i].contains(ALLOW_MARKER)
        };
        if annot(index) || (index > 0 && annot(index - 1)) || annot(index + 1) {
            continue;
        }
        if trimmed.starts_with("//") {
            continue;
        }
        let code = strip_comments(raw).trim();
        for marker in BANNED_MARKERS {
            if code.contains(marker) {
                findings.push(format!(
                    "{}:{}: banned marker '{}'",
                    path.display(),
                    index + 1,
                    marker
                ));
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
                path.display(),
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

    fn write_temp(name: &str, contents: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("llxprt-compat-gate-{}-{name}", std::process::id()));
        fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn flags_banned_legacy_reader() {
        let path = write_temp(
            "legacy-marker",
            "fn read_legacy_state(dir: &openat::Dir) {}\n",
        );
        let mut findings = Vec::new();
        scan_file(&path, &mut findings).unwrap();
        fs::remove_file(&path).unwrap();
        assert_eq!(findings.len(), 1);
        assert!(findings[0].contains("banned marker 'legacy'"));
    }

    #[test]
    fn allows_marker_with_reason() {
        let path = write_temp(
            "allow-marker",
            "    read_legacy_state(dir)  // compat-allow: issue 74 documented exception\n",
        );
        let mut findings = Vec::new();
        scan_file(&path, &mut findings).unwrap();
        fs::remove_file(&path).unwrap();
        assert!(findings.is_empty());
    }

    #[test]
    fn flags_format_fallback_shape() {
        let path = write_temp(
            "fallback-shape",
            "match serde_json::from_slice::<StateSlot>(&bytes) {\n    Ok(slot) => slot,\n    Err(_) => {\n        match serde_json::from_slice::<SessionState>(&bytes) {\n            Ok(state) => state,\n            Err(_) => return Err(Corrupt),\n        }\n    }\n}\n",
        );
        let mut findings = Vec::new();
        scan_file(&path, &mut findings).unwrap();
        fs::remove_file(&path).unwrap();
        assert!(
            findings.iter().any(|f| f.contains("format-fallback shape")),
            "findings: {findings:?}"
        );
    }

    #[test]
    fn skips_error_guards_that_exit_before_parsing() {
        let path = write_temp(
            "guard-exit",
            concat!(
                "if reader.read_exact(&mut body).is_err() {\n",
                "    return;\n",
                "}\n",
                "let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);\n"
            ),
        );
        let mut findings = Vec::new();
        scan_file(&path, &mut findings).unwrap();
        fs::remove_file(&path).unwrap();
        assert!(
            !findings.iter().any(|f| f.contains("format-fallback shape")),
            "guard-exit followed by an unrelated parse must not flag: {findings:?}"
        );
    }
}
