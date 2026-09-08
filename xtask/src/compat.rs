//! Gate: production sources stay one-directional. No legacy readers,
//! migrations, or compatibility shims (issues 74 and 234).

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Marker a source line may carry to allow one flagged finding. The cited
/// issue number must be present in `xtask/compat-allowlist`, and the file
/// must carry a row in `compat-ledger.md`.
const ALLOW_MARKER: &str = "// compat-allow(#";

/// The pre-hardening marker form; kept spelled out so it fails loudly.
const BARE_MARKER: &str = "// compat-allow:";

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
    let allowed = load_allowlist(root)?;
    let ledger = load_ledger_paths(root)?;
    let files = production_sources(root)?;
    let mut findings = Vec::new();
    let mut marked: HashSet<String> = HashSet::new();
    for file in &files {
        let rel = file
            .strip_prefix(root)
            .map_err(|_| format!("path {} escapes root", file.display()))?
            .to_string_lossy()
            .into_owned();
        scan_file(file, &rel, &allowed, &ledger, &mut findings)?;
        let content = fs::read_to_string(file)
            .map_err(|error| format!("read {}: {error}", file.display()))?;
        if content.contains(ALLOW_MARKER) {
            marked.insert(rel);
        }
    }
    for rel in &ledger {
        if !marked.contains(rel) {
            findings.push(format!(
                "compat-ledger.md row for {rel} has no compat-allow marker"
            ));
        }
    }
    findings.sort();
    if findings.is_empty() {
        println!(
            "compat gate passed: {} production files scanned, {} exceptions allowlisted",
            files.len(),
            marked.len()
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

/// Maintainer-owned issue numbers allowed inside `compat-allow(#N)` markers.
fn load_allowlist(root: &Path) -> Result<HashSet<u64>, String> {
    let path = root.join("xtask").join("compat-allowlist");
    let content = fs::read_to_string(&path).map_err(|error| {
        format!(
            "read {}: {error} (maintainer-owned; create it, an empty file means zero exceptions)",
            path.display()
        )
    })?;
    let mut set = HashSet::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let issue: u64 = trimmed.parse().map_err(|_| {
            format!(
                "{}: entry {trimmed:?} is not an issue number",
                path.display()
            )
        })?;
        set.insert(issue);
    }
    Ok(set)
}

/// Repo-relative file paths named by `compat-ledger.md` rows.
fn load_ledger_paths(root: &Path) -> Result<HashSet<String>, String> {
    let path = root.join("compat-ledger.md");
    let content =
        fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let mut set = HashSet::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') {
            continue;
        }
        let cell = trimmed.trim_start_matches('|').trim();
        let Some((site, _rest)) = cell.split_once('|') else {
            continue;
        };
        let site = site.trim();
        if let Some(index) = site.rfind(':') {
            let file = &site[..index];
            if file.ends_with(".rs") {
                set.insert(file.to_string());
            }
        }
    }
    Ok(set)
}

fn strip_comments(line: &str) -> &str {
    line.split("//").next().unwrap_or("")
}

fn validate_marker(
    raw: &str,
    rel: &str,
    allowed: &HashSet<u64>,
    ledger: &HashSet<String>,
    findings: &mut Vec<String>,
) {
    if raw.contains(BARE_MARKER) && !raw.contains(ALLOW_MARKER) {
        findings.push(format!(
            "{rel}: bare compat-allow markers are no longer accepted; cite an issue as // compat-allow(#N): reason"
        ));
        return;
    }
    let Some(position) = raw.find(ALLOW_MARKER) else {
        return;
    };
    let rest = &raw[position + ALLOW_MARKER.len()..];
    let Some(close) = rest.find(')') else {
        findings.push(format!(
            "{rel}: malformed compat-allow marker, expected // compat-allow(#N): reason"
        ));
        return;
    };
    let issue: u64 = match rest[..close].trim().parse() {
        Ok(issue) => issue,
        Err(_) => {
            findings.push(format!(
                "{rel}: malformed compat-allow marker, issue number expected"
            ));
            return;
        }
    };
    if !allowed.contains(&issue) {
        findings.push(format!(
            "{rel}: compat-allow cites #{issue} which is not in xtask/compat-allowlist"
        ));
        return;
    }
    if !ledger.contains(rel) {
        findings.push(format!(
            "{rel}: compat-allow marker but no compat-ledger.md row for this file"
        ));
    }
}

fn scan_file(
    path: &Path,
    rel: &str,
    allowed: &HashSet<u64>,
    ledger: &HashSet<String>,
    findings: &mut Vec<String>,
) -> Result<(), String> {
    let content =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let lines: Vec<&str> = content.lines().collect();
    for (index, raw) in lines.iter().enumerate() {
        let trimmed = raw.trim();
        validate_marker(raw, rel, allowed, ledger, findings);
        let is_marker = |line: &str| line.contains(ALLOW_MARKER) || line.contains(BARE_MARKER);
        if is_marker(raw)
            || (index > 0 && is_marker(lines[index - 1]))
            || lines.get(index + 1).is_some_and(|next| is_marker(next))
        {
            continue;
        }
        if trimmed.starts_with("//") {
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

    fn write_temp(name: &str, contents: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("llxprt-compat-gate-{}-{name}", std::process::id()));
        fs::write(&path, contents).unwrap();
        path
    }

    fn sets(issue: u64, rel: &str) -> (HashSet<u64>, HashSet<String>) {
        let mut allowed = HashSet::new();
        allowed.insert(issue);
        let mut ledger = HashSet::new();
        ledger.insert(rel.to_string());
        (allowed, ledger)
    }

    #[test]
    fn flags_banned_legacy_reader() {
        let path = write_temp(
            "legacy-marker",
            "fn read_legacy_state(dir: &openat::Dir) {}\n",
        );
        let (allowed, ledger) = sets(74, "src/x.rs");
        let mut findings = Vec::new();
        scan_file(&path, "src/x.rs", &allowed, &ledger, &mut findings).unwrap();
        fs::remove_file(&path).unwrap();
        assert_eq!(findings.len(), 1);
        assert!(findings[0].contains("banned marker 'legacy'"));
    }

    #[test]
    fn allows_cited_marker_when_allowlisted_and_ledgered() {
        let path = write_temp(
            "allow-marker",
            "    read_legacy_state(dir)  // compat-allow(#74): documented exception\n",
        );
        let (allowed, ledger) = sets(74, "src/x.rs");
        let mut findings = Vec::new();
        scan_file(&path, "src/x.rs", &allowed, &ledger, &mut findings).unwrap();
        fs::remove_file(&path).unwrap();
        assert!(findings.is_empty(), "findings: {findings:?}");
    }

    #[test]
    fn bare_marker_is_a_finding() {
        let path = write_temp(
            "bare-marker",
            "    read_legacy_state(dir)  // compat-allow: old form\n",
        );
        let (allowed, ledger) = sets(74, "src/x.rs");
        let mut findings = Vec::new();
        scan_file(&path, "src/x.rs", &allowed, &ledger, &mut findings).unwrap();
        fs::remove_file(&path).unwrap();
        assert!(findings
            .iter()
            .any(|f| f.contains("bare compat-allow markers are no longer accepted")));
    }

    #[test]
    fn cited_issue_must_be_allowlisted() {
        let path = write_temp(
            "unlisted-issue",
            "    read_legacy_state(dir)  // compat-allow(#999): reason\n",
        );
        let (allowed, ledger) = sets(74, "src/x.rs");
        let mut findings = Vec::new();
        scan_file(&path, "src/x.rs", &allowed, &ledger, &mut findings).unwrap();
        fs::remove_file(&path).unwrap();
        assert!(findings
            .iter()
            .any(|f| f.contains("#999 which is not in xtask/compat-allowlist")));
    }

    #[test]
    fn cited_file_must_have_a_ledger_row() {
        let path = write_temp(
            "no-ledger-row",
            "    read_legacy_state(dir)  // compat-allow(#74): reason\n",
        );
        let (allowed, _) = sets(74, "src/x.rs");
        let empty_ledger = HashSet::new();
        let mut findings = Vec::new();
        scan_file(&path, "src/x.rs", &allowed, &empty_ledger, &mut findings).unwrap();
        fs::remove_file(&path).unwrap();
        assert!(findings
            .iter()
            .any(|f| f.contains("no compat-ledger.md row for this file")));
    }

    #[test]
    fn flags_format_fallback_shape() {
        let path = write_temp(
            "fallback-shape",
            "match serde_json::from_slice::<StateSlot>(&bytes) {\n    Ok(slot) => slot,\n    Err(_) => {\n        match serde_json::from_slice::<SessionState>(&bytes) {\n            Ok(state) => state,\n            Err(_) => return Err(Corrupt),\n        }\n    }\n}\n",
        );
        let (allowed, ledger) = sets(74, "src/x.rs");
        let mut findings = Vec::new();
        scan_file(&path, "src/x.rs", &allowed, &ledger, &mut findings).unwrap();
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
        let (allowed, ledger) = sets(74, "src/x.rs");
        let mut findings = Vec::new();
        scan_file(&path, "src/x.rs", &allowed, &ledger, &mut findings).unwrap();
        fs::remove_file(&path).unwrap();
        assert!(
            !findings.iter().any(|f| f.contains("format-fallback shape")),
            "guard-exit followed by an unrelated parse must not flag: {findings:?}"
        );
    }
}
