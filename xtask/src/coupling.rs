//! Production-module dependency graph and coupling debt gate.
//!
//! This deliberately uses a small lexer rather than a Rust parser: module dependencies are
//! determined from absolute `crate::module` paths after comments, strings, and test-only code
//! are removed. The repository's production modules are the public and private top-level modules declared by
//! `src/lib.rs`; all files belonging to each of those modules are discovered recursively.

use crate::coupling_graph::{adjacency, minimum_feedback_arc_set, strongly_connected_components};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const LEDGER_PATH: &str = "xtask/coupling-ledger.tsv";
pub const OWNER_CHECK_SCRIPT: &str = "scripts/validate-coupling-owners.py";

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct Edge {
    pub(super) from: String,
    pub(super) to: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Debt {
    edge: Edge,
    issue: u64,
}

#[derive(Debug)]
pub(super) struct Graph {
    pub(super) modules: BTreeSet<String>,
    pub(super) edges: BTreeSet<Edge>,
}

/// Options for the standalone, read-only coupling gate.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CheckOptions {
    /// Git revision whose ledger bounds ordinary checked-in debt.
    pub base_ref: Option<String>,
    /// Explicit permission for pre-committed growth relative to `base_ref`.
    pub accept_new_coupling: bool,
    /// Run the fail-closed GitHub owner validation boundary.
    pub owner_check: bool,
}

/// Analyze production dependencies and enforce the current ledger without mutation or network.
pub fn run(workspace: &Path) -> Result<(), String> {
    run_with_options(workspace, &CheckOptions::default())
}

/// Analyze dependencies with explicit base-comparison and owner-validation policy.
pub fn run_with_options(workspace: &Path, options: &CheckOptions) -> Result<(), String> {
    if options.accept_new_coupling && options.base_ref.is_none() {
        return Err("--accept-new-coupling requires --base-ref <REF>".into());
    }
    if options.accept_new_coupling && !options.owner_check {
        return Err(
            "--accept-new-coupling requires --owner-check so every owner is verified open".into(),
        );
    }
    if options.accept_new_coupling {
        require_clean_committed_worktree(workspace)?;
    }

    let ledger_path = workspace.join(LEDGER_PATH);
    let ledger = read_ledger(&ledger_path)?;
    let base = match &options.base_ref {
        Some(reference) => read_base_ledger(workspace, reference)?,
        None => None,
    };
    enforce(
        &workspace.join("src"),
        &ledger,
        base.as_deref(),
        options.accept_new_coupling,
    )?;

    if options.owner_check {
        validate_owners(workspace, &ledger_path, &ledger)?;
    }
    Ok(())
}

fn enforce(
    source: &Path,
    ledger: &[Debt],
    base: Option<&[Debt]>,
    accept_new_coupling: bool,
) -> Result<(), String> {
    let graph = analyze(source)?;
    let debt: BTreeSet<Edge> = ledger.iter().map(|item| item.edge.clone()).collect();
    let feedback = minimum_feedback_arc_set(&graph);
    let cycle_edges = &feedback.edges;
    let unowned: Vec<_> = cycle_edges.difference(&debt).cloned().collect();
    let stale: Vec<_> = debt.difference(cycle_edges).cloned().collect();

    report(&graph, cycle_edges, ledger, &unowned, &stale);

    if !unowned.is_empty() {
        return Err(format!(
            "coupling gate failed: {} feedback edge(s) have no debt-ledger row; check in a row with an open owner issue (ledger growth additionally requires --accept-new-coupling)",
            unowned.len()
        ));
    }
    if !stale.is_empty() {
        return Err(format!(
            "coupling gate failed: {} stale debt-ledger row(s) are no longer in the deterministic minimum feedback set; remove those rows",
            stale.len()
        ));
    }

    if let Some(base) = base {
        let current: BTreeSet<_> = ledger.iter().map(|debt| debt.edge.clone()).collect();
        let old: BTreeSet<_> = base.iter().map(|debt| debt.edge.clone()).collect();
        let additions: Vec<_> = current.difference(&old).collect();
        if !additions.is_empty() && !accept_new_coupling {
            return Err(format!(
                "coupling gate failed: checked-in debt ledger grew by {} row(s) relative to the base; ordinary CI permits only shrinkage (an exceptional addition requires --accept-new-coupling and open-owner validation)",
                additions.len()
            ));
        }
    }
    Ok(())
}

fn git_output(
    workspace: &Path,
    args: &[&str],
    context: &str,
) -> Result<std::process::Output, String> {
    Command::new("git")
        .current_dir(workspace)
        .args(args)
        .output()
        .map_err(|error| format!("failed to run git while {context}: {error}"))
}

fn command_error(context: &str, output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr.trim();
    if detail.is_empty() {
        format!("{context} (git exited with {})", output.status)
    } else {
        format!("{context}: {detail}")
    }
}

fn require_clean_committed_worktree(workspace: &Path) -> Result<(), String> {
    let head = git_output(
        workspace,
        &["rev-parse", "--verify", "HEAD^{commit}"],
        "resolving the candidate commit",
    )?;
    if !head.status.success() {
        return Err(command_error(
            "acceptance requires the candidate to be committed at HEAD",
            &head,
        ));
    }

    // Normally ignored verification output is deliberately absent from porcelain output.
    // Everything else, including untracked files, proves that HEAD is not the candidate.
    let status = git_output(
        workspace,
        &["status", "--porcelain=v1", "--untracked-files=normal"],
        "checking whether the acceptance worktree is clean",
    )?;
    if !status.status.success() {
        return Err(command_error(
            "acceptance could not verify a clean Git worktree",
            &status,
        ));
    }
    if !status.stdout.is_empty() {
        return Err(
            "--accept-new-coupling requires a committed candidate and a clean Git worktree"
                .to_owned(),
        );
    }
    Ok(())
}

fn read_ledger(path: &Path) -> Result<Vec<Debt>, String> {
    let source = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    parse_ledger(&source, &path.display().to_string())
}

/// Accepts only a regular-file blob entry (`100644`/`100755`) for the ledger path;
/// symlinks (`120000`) and submodules/gitlinks (`160000`) are rejected.
fn parse_tree_record<'a>(record: &'a str, commit: &str) -> Result<(&'a str, &'a str), String> {
    let (metadata, path) = record
        .split_once('\t')
        .ok_or_else(|| format!("malformed Git tree result for {commit}:{LEDGER_PATH}"))?;
    let mut fields = metadata.split(' ');
    let mode = fields.next();
    let kind = fields.next();
    let object_id = fields.next();
    if fields.next().is_some()
        || mode.is_none()
        || mode != Some("100644") && mode != Some("100755")
        || kind != Some("blob")
        || object_id.is_none()
        || path != LEDGER_PATH
    {
        return Err(format!(
            "unexpected non-blob Git tree entry for {commit}:{LEDGER_PATH}"
        ));
    }
    Ok((path, object_id.expect("checked object id")))
}

fn read_base_ledger(workspace: &Path, reference: &str) -> Result<Option<Vec<Debt>>, String> {
    if reference.is_empty() || reference.starts_with('-') {
        return Err(format!("invalid base ref `{reference}`"));
    }

    let requested = format!("{reference}^{{commit}}");
    let resolved = git_output(
        workspace,
        &["rev-parse", "--verify", &requested],
        &format!("resolving base ref `{reference}`"),
    )?;
    if !resolved.status.success() {
        return Err(command_error(
            &format!("base ref `{reference}` is not an available commit"),
            &resolved,
        ));
    }
    let commit = std::str::from_utf8(&resolved.stdout)
        .map_err(|_| format!("git returned a non-UTF-8 object id for base ref `{reference}`"))?
        .trim();
    if commit.is_empty() || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!(
            "git returned an invalid commit id for base ref `{reference}`"
        ));
    }

    let tree = git_output(
        workspace,
        &["ls-tree", "-z", "--full-tree", commit, "--", LEDGER_PATH],
        &format!("querying {commit} for {LEDGER_PATH}"),
    )?;
    if !tree.status.success() {
        return Err(command_error(
            &format!("failed to inspect {commit}:{LEDGER_PATH}"),
            &tree,
        ));
    }
    if tree.stdout.is_empty() {
        println!("coupling ledger base: absent (initial seed permitted)");
        return Ok(None);
    }

    let records: Vec<&[u8]> = tree.stdout.split(|byte| *byte == 0).collect();
    if records.len() != 2 || !records[1].is_empty() {
        return Err(format!(
            "unexpected Git tree result for {commit}:{LEDGER_PATH}"
        ));
    }
    let record = std::str::from_utf8(records[0])
        .map_err(|_| format!("non-UTF-8 Git tree result for {commit}:{LEDGER_PATH}"))?;
    let (path, object_id) = parse_tree_record(record, commit)?;
    debug_assert_eq!(path, LEDGER_PATH);
    debug_assert!(!object_id.is_empty());

    let object = format!("{commit}:{LEDGER_PATH}");
    let blob = git_output(
        workspace,
        &["cat-file", "blob", &object],
        &format!("reading {object}"),
    )?;
    if !blob.status.success() {
        return Err(command_error(&format!("failed to read {object}"), &blob));
    }
    let text = String::from_utf8(blob.stdout).map_err(|_| format!("{object} is not UTF-8"))?;
    parse_ledger(&text, &object).map(Some)
}

fn validate_owners(workspace: &Path, ledger_path: &Path, ledger: &[Debt]) -> Result<(), String> {
    let output = Command::new("python3")
        .current_dir(workspace)
        .arg(OWNER_CHECK_SCRIPT)
        .args(["--ledger", &ledger_path.to_string_lossy()])
        .output()
        .map_err(|error| format!("failed to start coupling owner validation: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "coupling owner validation failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let status = String::from_utf8(output.stdout)
        .map_err(|_| "coupling owner validator emitted non-UTF-8 output".to_string())?;
    check_owner_handoff(ledger, &status)?;
    println!(
        "coupling owners: {} distinct open issue(s) verified",
        owner_ids(ledger).len()
    );
    Ok(())
}

fn owner_ids(ledger: &[Debt]) -> BTreeSet<u64> {
    ledger.iter().map(|debt| debt.issue).collect()
}

fn check_owner_handoff(ledger: &[Debt], status: &str) -> Result<(), String> {
    let expected = owner_ids(ledger);
    let mut open = BTreeSet::new();
    for (index, line) in status.lines().enumerate() {
        let Some((issue, state)) = line.split_once('\t') else {
            return Err(format!(
                "invalid owner-validation output line {}",
                index + 1
            ));
        };
        let issue = issue.parse::<u64>().map_err(|_| {
            format!(
                "invalid owner issue in validation output line {}",
                index + 1
            )
        })?;
        if issue == 0 || state != "open" || !open.insert(issue) {
            return Err(format!(
                "fail-closed owner validation for issue #{issue}: expected one `open` result"
            ));
        }
    }
    if open != expected {
        let missing: Vec<_> = expected.difference(&open).copied().collect();
        let extra: Vec<_> = open.difference(&expected).copied().collect();
        return Err(format!(
            "owner-validation handoff did not exactly match ledger owners (missing: {missing:?}, extra: {extra:?})"
        ));
    }
    Ok(())
}

fn analyze(source: &Path) -> Result<Graph, String> {
    let lib = source.join("lib.rs");
    let lib_source = fs::read_to_string(&lib)
        .map_err(|error| format!("failed to read {}: {error}", lib.display()))?;
    let clean_lib = production_text(&lib_source);
    let declarations = declared_declarations(&clean_lib);
    let modules: BTreeSet<String> = declarations.keys().cloned().collect();
    if modules.is_empty() {
        return Err(format!("no production modules found in {}", lib.display()));
    }

    let mut edges = BTreeSet::new();
    for (module, declaration) in &declarations {
        let mut texts = Vec::<String>::new();
        let mut scanned: BTreeSet<PathBuf> = BTreeSet::new();
        let mut queue: Vec<PathBuf> = Vec::new();

        let module_file = source.join(format!("{module}.rs"));
        let module_directory = source.join(module);
        match declaration {
            // The inline body is always scanned, even when `src/<module>/` also holds
            // external descendants, and never replaced by a stray `src/<module>.rs`.
            Declaration::Inline => {
                let inline = inline_module_body(&clean_lib, module).ok_or_else(|| {
                    format!(
                        "module `{module}` has an unbalanced inline body under {}",
                        source.display()
                    )
                })?;
                texts.push(inline.to_owned());
                // External descendants still count: every `.rs` under `src/<module>/` is
                // reachable from an inline declaration through `mod <child>;`.
                collect_files(&module_directory, &mut queue);
            }
            Declaration::External => {
                if module_file.is_file() {
                    queue.push(module_file);
                }
                collect_files(&module_directory, &mut queue);
            }
        }

        // File-level scan of the module's whole source tree: every `.rs` file under
        // `src/<module>/` belongs to the module (recursively, no name exclusions).
        // Non-production content is excluded semantically: cfg(test)-impossible items and
        // `#[test]`-attributed functions are blanked by `production_text`, so a file whose
        // only `crate::` references live inside tests contributes nothing.
        while let Some(file) = queue.pop() {
            if !scanned.insert(file.clone()) {
                continue;
            }
            let raw = fs::read_to_string(&file)
                .map_err(|error| format!("failed to read {}: {error}", file.display()))?;
            texts.push(raw);
        }

        if texts.is_empty() {
            return Err(format!(
                "module `{module}` has no source file or inline body under {}",
                source.display()
            ));
        }
        for raw in texts {
            let clean = production_text(&raw);
            for target in crate_paths(&clean, &modules) {
                if &target != module {
                    edges.insert(Edge {
                        from: module.clone(),
                        to: target,
                    });
                }
            }
        }
    }

    Ok(Graph { modules, edges })
}

/// Recursively enqueue every `.rs` file under `directory` (which may not exist). Discovery is
/// file-level: no name exclusions, because test-only content is blanked semantically.
fn collect_files(directory: &Path, queue: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, queue);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            queue.push(path);
        }
    }
}

/// One top-level declaration in a Rust source file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Declaration {
    /// `mod name { ... }`: the body is inline in the declaring file.
    Inline,
    /// `mod name;`: the body lives in an external file or directory.
    External,
}

/// Classify every top-level declaration, distinguishing an inline body from an external
/// semicolon declaration so a coincidentally present `src/<module>.rs` can never stand in for
/// an inline declaration. Only the declaration's own trailing delimiter decides, so an
/// external `mod foo;` cannot borrow a brace from a later item.
fn declared_declarations(source: &str) -> BTreeMap<String, Declaration> {
    let source = production_text(source);
    let bytes = source.as_bytes();
    let mut result = BTreeMap::new();
    let mut delimiters = Vec::new();
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'{' | b'(' | b'[' => {
                delimiters.push(bytes[index]);
                index += 1;
            }
            b'}' | b')' | b']' => {
                delimiters.pop();
                index += 1;
            }
            byte if is_identifier_start(byte) => {
                let start = index;
                index += 1;
                while index < bytes.len() && is_identifier_continue(bytes[index]) {
                    index += 1;
                }
                if delimiters.is_empty() && &source[start..index] == "mod" {
                    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
                        index += 1;
                    }
                    let name_start = index;
                    if index < bytes.len() && is_identifier_start(bytes[index]) {
                        index += 1;
                        while index < bytes.len() && is_identifier_continue(bytes[index]) {
                            index += 1;
                        }
                        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
                            index += 1;
                        }
                        let kind = match bytes.get(index) {
                            Some(b'{') => Declaration::Inline,
                            _ => Declaration::External,
                        };
                        result.insert(source[name_start..index].trim().to_owned(), kind);
                    }
                }
            }
            _ => index += 1,
        }
    }
    result
}

fn is_identifier_start(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphabetic()
}

fn is_identifier_continue(byte: u8) -> bool {
    is_identifier_start(byte) || byte.is_ascii_digit()
}

/// Extract the body of a top-level inline `mod name { ... }` declaration.
///
/// The scan matches the exact `mod name {` token sequence, so an external `mod name;`
/// declaration can never borrow the brace of a later item, and a body whose first item is
/// itself a `mod` declaration is still attributed to the governing module.
fn inline_module_body<'a>(source: &'a str, module: &str) -> Option<&'a str> {
    let bytes = source.as_bytes();
    let mut index = 0;
    let mut delimiters = Vec::new();
    while index < bytes.len() {
        match bytes[index] {
            b'{' | b'(' | b'[' => {
                delimiters.push(bytes[index]);
                index += 1;
            }
            b'}' | b')' | b']' => {
                delimiters.pop();
                index += 1;
            }
            byte if is_identifier_start(byte) => {
                let start = index;
                index += 1;
                while index < bytes.len() && is_identifier_continue(bytes[index]) {
                    index += 1;
                }
                if &source[start..index] != "mod" {
                    continue;
                }
                while index < bytes.len() && bytes[index].is_ascii_whitespace() {
                    index += 1;
                }
                let name_start = index;
                if index < bytes.len() && is_identifier_start(bytes[index]) {
                    index += 1;
                    while index < bytes.len() && is_identifier_continue(bytes[index]) {
                        index += 1;
                    }
                } else {
                    continue;
                }
                if &source[name_start..index] != module {
                    continue;
                }
                while index < bytes.len() && bytes[index].is_ascii_whitespace() {
                    index += 1;
                }
                if index >= bytes.len() || bytes[index] != b'{' {
                    // An external `mod name;` declaration: keep scanning for the inline form.
                    continue;
                }
                if !delimiters.is_empty() {
                    // A same-named module nested inside another item can never be the
                    // top-level declaration whose body is wanted here.
                    continue;
                }
                index += 1;
                let body_start = index;
                let mut depth = 1usize;
                while index < bytes.len() {
                    match bytes[index] {
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                return Some(&source[body_start..index]);
                            }
                        }
                        _ => {}
                    }
                    index += 1;
                }
                return None;
            }
            _ => index += 1,
        }
    }
    None
}

/// Return the top-level modules referenced by absolute `crate::module` paths.
fn crate_paths(source: &str, modules: &BTreeSet<String>) -> BTreeSet<String> {
    let tokens = identifiers(source);
    let mut result = BTreeSet::new();
    let mut index = 0;
    while index < tokens.len() {
        if tokens[index] == "crate" && index + 2 < tokens.len() && tokens[index + 1] == "::" {
            index += 2;
            if tokens[index] == "{" {
                let mut depth = 1usize;
                let mut expect_head = true;
                index += 1;
                while index < tokens.len() && depth > 0 {
                    match tokens[index].as_str() {
                        "{" => depth += 1,
                        "}" => depth -= 1,
                        "," if depth == 1 => expect_head = true,
                        token if depth == 1 && expect_head => {
                            if modules.contains(token) {
                                result.insert(token.to_owned());
                            }
                            expect_head = false;
                        }
                        _ => {}
                    }
                    index += 1;
                }
                continue;
            }
            if modules.contains(&tokens[index]) {
                result.insert(tokens[index].clone());
            }
        }
        index += 1;
    }
    result
}

/// Return identifiers and path/group punctuation needed to recognize absolute crate paths.
fn identifiers(source: &str) -> Vec<String> {
    let mut result = Vec::new();
    let bytes = source.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index].is_ascii_alphabetic() || bytes[index] == b'_' {
            let start = index;
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            result.push(source[start..index].to_owned());
        } else if bytes[index..].starts_with(b"::") {
            result.push("::".to_owned());
            index += 2;
        } else if matches!(bytes[index], b'{' | b'}' | b',' | b';') {
            result.push((bytes[index] as char).to_string());
            index += 1;
        } else {
            index += 1;
        }
    }
    result
}

/// Blank comments, literals, and items immediately governed by `#[cfg(...)]`.
fn production_text(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut clean = bytes.to_vec();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index..].starts_with(b"//") {
            let end = bytes[index..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes.len(), |relative| index + relative);
            blank(&mut clean, index, end);
            index = end;
        } else if bytes[index..].starts_with(b"/*") {
            let start = index;
            index += 2;
            let mut depth = 1usize;
            while index < bytes.len() && depth > 0 {
                if bytes[index..].starts_with(b"/*") {
                    depth += 1;
                    index += 2;
                } else if bytes[index..].starts_with(b"*/") {
                    depth -= 1;
                    index += 2;
                } else {
                    index += 1;
                }
            }
            blank(&mut clean, start, index);
        } else if let Some((quote, hashes)) = raw_string_start(bytes, index) {
            let start = index;
            index = quote + 1;
            while index < bytes.len() {
                if bytes[index] == b'"'
                    && index + 1 + hashes <= bytes.len()
                    && bytes[index + 1..index + 1 + hashes]
                        .iter()
                        .all(|byte| *byte == b'#')
                {
                    index += 1 + hashes;
                    break;
                }
                index += 1;
            }
            blank(&mut clean, start, index);
        } else if bytes[index] == b'"'
            || ((bytes[index] == b'b' || bytes[index] == b'c')
                && bytes.get(index + 1) == Some(&b'"'))
        {
            let start = index;
            if bytes[index] == b'b' || bytes[index] == b'c' {
                index += 1;
            }
            index = quoted_end(bytes, index, b'"');
            blank(&mut clean, start, index);
        } else if (bytes[index] == b'\''
            || ((bytes[index] == b'b' || bytes[index] == b'c')
                && bytes.get(index + 1) == Some(&b'\'')))
            && char_literal_end(bytes, index).is_some()
        {
            let start = index;
            index = char_literal_end(bytes, index).expect("checked above");
            blank(&mut clean, start, index);
        } else {
            index += 1;
        }
    }
    let clean = String::from_utf8(clean).expect("blanking preserves UTF-8");
    remove_test_items(&clean)
}

/// Blank a byte range, preserving newlines so line/column positions stay stable.
fn blank(bytes: &mut [u8], start: usize, end: usize) {
    for byte in &mut bytes[start..end] {
        if *byte != b'\n' {
            *byte = b' ';
        }
    }
}

/// Recognise `r"…"`, `br"…"`, `cr"…"` and every hash count (`r#"…"#`, `r##"…"##`, ...).
/// Returns the quote index and the hash count.
fn raw_string_start(bytes: &[u8], index: usize) -> Option<(usize, usize)> {
    let mut i = index;
    if bytes.get(i) == Some(&b'b') || bytes.get(i) == Some(&b'c') {
        i += 1;
    }
    if bytes.get(i) != Some(&b'r') {
        return None;
    }
    i += 1;
    let hash_start = i;
    while bytes.get(i) == Some(&b'#') {
        i += 1;
    }
    (bytes.get(i) == Some(&b'"')).then_some((i, i - hash_start))
}

/// End of an ordinary (escape-aware) string that starts with the quote at `quote`.
fn quoted_end(bytes: &[u8], quote: usize, delimiter: u8) -> usize {
    let mut i = quote + 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i = (i + 2).min(bytes.len());
        } else if bytes[i] == delimiter {
            return i + 1;
        } else {
            i += 1;
        }
    }
    bytes.len()
}

/// End of a `'\u{…}'`, `'x'`, `'\n'` or `b'x'` char literal, or `None` when the bytes at
/// `index` are not a well-formed char literal (a bare newline or quote cannot occur).
fn char_literal_end(bytes: &[u8], index: usize) -> Option<usize> {
    let quote = if bytes.get(index) == Some(&b'b') || bytes.get(index) == Some(&b'c') {
        index + 1
    } else {
        index
    };
    if bytes.get(quote) != Some(&b'\'') {
        return None;
    }
    let mut i = quote + 1;
    if bytes.get(i) == Some(&b'\\') {
        i += 1;
        if bytes.get(i) == Some(&b'u') && bytes.get(i + 1) == Some(&b'{') {
            i += 2;
            while bytes.get(i).is_some_and(|byte| *byte != b'}') {
                i += 1;
            }
            if bytes.get(i) != Some(&b'}') {
                return None;
            }
            i += 1;
        } else {
            i += 1;
        }
    } else {
        let ch = std::str::from_utf8(&bytes[i..]).ok()?.chars().next()?;
        if ch == '\n' || ch == '\'' {
            return None;
        }
        i += ch.len_utf8();
    }
    (bytes.get(i) == Some(&b'\'')).then_some(i + 1)
}

/// Blank whole items that can never be selected with `test` fixed to false, plus the
/// `#[test]` / `#[name::test]` test functions themselves. Both operate on lexer-cleaned
/// text, so string and comment contents never influence the decision.
fn remove_test_items(source: &str) -> String {
    let blanked = remove_nonproduction_cfg_items(source);
    let blanked = blank_attribute_items(&blanked, b"#[test]", true);
    blank_attribute_items(&blanked, b"::test]", false)
}

/// Blank every `#[cfg(...)]` item whose cfg cannot be true when `test` is false, while
/// retaining production-capable predicates such as `cfg(any(test, feature = "x"))`.
fn remove_nonproduction_cfg_items(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut result = bytes.to_vec();
    let mut i = 0;
    while i + 2 <= bytes.len() {
        if bytes[i..].starts_with(b"#[") {
            let mut end = i + 2;
            let mut depth = 1;
            while end < bytes.len() && depth > 0 {
                match bytes[end] {
                    b'[' => depth += 1,
                    b']' => depth -= 1,
                    _ => {}
                }
                end += 1;
            }
            let attribute = &source[i + 2..end.saturating_sub(1)];
            if cfg_cannot_be_production(attribute) {
                let mut item = end;
                while item < bytes.len() && bytes[item].is_ascii_whitespace() {
                    item += 1;
                }
                let finish = item_end(bytes, item);
                blank(&mut result, i, finish);
                i = finish;
                continue;
            }
            i = end;
        } else {
            i += 1;
        }
    }
    String::from_utf8(result).expect("blanking preserves UTF-8")
}

/// True when the attribute body is a `cfg(...)` predicate that is false for `test = false`.
fn cfg_cannot_be_production(attribute: &str) -> bool {
    let compact: String = attribute.chars().filter(|c| !c.is_whitespace()).collect();
    let Some(inner) = compact
        .strip_prefix("cfg(")
        .and_then(|s| s.strip_suffix(')'))
    else {
        return false;
    };
    !cfg_possibilities(inner).0
}

/// `(can_be_true, can_be_false)` when `test` is fixed to false.
fn cfg_possibilities(expr: &str) -> (bool, bool) {
    if expr == "test" {
        return (false, true);
    }
    for (name, combine) in [("all", true), ("any", false)] {
        if let Some(inner) = expr
            .strip_prefix(&format!("{name}("))
            .and_then(|s| s.strip_suffix(')'))
        {
            let parts = split_cfg_args(inner);
            if combine {
                return (
                    parts.iter().all(|part| cfg_possibilities(part).0),
                    parts.iter().any(|part| cfg_possibilities(part).1),
                );
            }
            return (
                parts.iter().any(|part| cfg_possibilities(part).0),
                parts.iter().all(|part| cfg_possibilities(part).1),
            );
        }
    }
    if let Some(inner) = expr.strip_prefix("not(").and_then(|s| s.strip_suffix(')')) {
        let (can_true, can_false) = cfg_possibilities(inner);
        return (can_false, can_true);
    }
    (true, true)
}

/// Split `a, b(c, d), e` on depth-zero commas.
fn split_cfg_args(expr: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    for (i, byte) in expr.bytes().enumerate() {
        match byte {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b',' if depth == 0 => {
                result.push(&expr[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < expr.len() {
        result.push(&expr[start..]);
    }
    result
}

fn blank_attribute_items(source: &str, needle: &[u8], needle_is_attribute: bool) -> String {
    let mut bytes = source.as_bytes().to_vec();
    let mut search = 0;
    while let Some(relative) = source.as_bytes()[search..]
        .windows(needle.len())
        .position(|window| window == needle)
    {
        let found = search + relative;
        let attribute_start = if needle_is_attribute {
            Some(found)
        } else {
            source[..found].rfind("#[")
        };
        let Some(attribute_start) = attribute_start else {
            search = found + needle.len();
            continue;
        };
        let Some(attribute_end_relative) = source[found..].find(']') else {
            break;
        };
        let item_start = found + attribute_end_relative + 1;
        let item_end = item_end(source.as_bytes(), item_start);
        for byte in &mut bytes[attribute_start..item_end] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
        search = item_end.max(found + needle.len());
    }
    String::from_utf8(bytes).expect("only ASCII bytes were blanked")
}

fn item_end(bytes: &[u8], start: usize) -> usize {
    let mut index = start;
    let mut braces = 0usize;
    let mut began_block = false;
    while index < bytes.len() {
        match bytes[index] {
            b'{' => {
                braces += 1;
                began_block = true;
            }
            b'}' if began_block => {
                braces -= 1;
                if braces == 0 {
                    return index + 1;
                }
            }
            b';' if !began_block => return index + 1,
            _ => {}
        }
        index += 1;
    }
    bytes.len()
}

/// Parse the ledger format: leading `#` comment lines and blank lines are ignored.
fn parse_ledger(source: &str, origin: &str) -> Result<Vec<Debt>, String> {
    let mut entries = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, line) in source.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 3 {
            return Err(format!(
                "{origin}:{}: expected FROM<TAB>TO<TAB>ISSUE",
                index + 1
            ));
        }
        let issue = fields[2]
            .parse::<u64>()
            .map_err(|_| format!("{origin}:{}: issue must be a positive number", index + 1))?;
        if issue == 0 || fields[0].is_empty() || fields[1].is_empty() {
            return Err(format!("{origin}:{}: invalid ledger entry", index + 1));
        }
        let edge = Edge {
            from: fields[0].to_owned(),
            to: fields[1].to_owned(),
        };
        if !seen.insert(edge.clone()) {
            return Err(format!(
                "{origin}:{}: duplicate edge {} -> {}",
                index + 1,
                edge.from,
                edge.to
            ));
        }
        entries.push(Debt { edge, issue });
    }
    entries.sort_by(|left, right| left.edge.cmp(&right.edge));
    Ok(entries)
}

fn report(
    graph: &Graph,
    cycle_edges: &BTreeSet<Edge>,
    ledger: &[Debt],
    unowned: &[Edge],
    stale: &[Edge],
) {
    let adjacent = adjacency(graph);
    println!(
        "coupling: {} production modules, {} dependencies, {} feedback edges, {} ledger entries",
        graph.modules.len(),
        graph.edges.len(),
        cycle_edges.len(),
        ledger.len()
    );
    println!("module dependencies:");
    for module in &graph.modules {
        let dependencies = adjacent[module].to_vec();
        println!(
            "  {module}: {}",
            if dependencies.is_empty() {
                "-".to_owned()
            } else {
                dependencies.join(", ")
            }
        );
    }
    let cyclic: Vec<_> = strongly_connected_components(graph)
        .into_iter()
        .filter(|component| component.len() > 1)
        .collect();
    if cyclic.is_empty() {
        println!("cyclic SCCs: none");
    } else {
        println!("cyclic SCCs ({}):", cyclic.len());
        for component in cyclic {
            println!("  [{}]", component.join(", "));
        }
    }
    println!("owned coupling debt ({}):", ledger.len());
    for item in ledger {
        let state = if graph.edges.contains(&item.edge) {
            "present"
        } else {
            "stale"
        };
        println!(
            "  {} -> {} (#{}; {state})",
            item.edge.from, item.edge.to, item.issue
        );
    }
    for edge in unowned {
        eprintln!("unowned feedback edge: {} -> {}", edge.from, edge.to);
    }
    for edge in stale {
        eprintln!("stale debt-ledger edge: {} -> {}", edge.from, edge.to);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);
            let unique = NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
            let root =
                env::temp_dir().join(format!("llxprt-coupling-{}-{unique}", std::process::id()));
            fs::create_dir_all(root.join("src")).unwrap();
            Self(root)
        }

        fn write(&self, relative: &str, source: &str) {
            let path = self.0.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, source).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn discovers_public_and_private_top_level_modules() {
        let source = r#"
            pub mod alpha;
            mod private;
            pub(crate) mod crate_visible;
            pub mod inline {
                pub(crate) mod scoped {}
            }
            fn function() { mod local {} }
            macro_rules! declarations { () => { mod generated {} } }
            const TEXT: &str = "mod in_a_string;";
            // mod in_a_comment;
            #[cfg(test)]
            mod tests_only;
        "#;
        let declarations = declared_declarations(source);
        let mut expected = BTreeMap::new();
        expected.insert("alpha".to_owned(), Declaration::External);
        expected.insert("private".to_owned(), Declaration::External);
        expected.insert("crate_visible".to_owned(), Declaration::External);
        expected.insert("inline".to_owned(), Declaration::Inline);
        assert_eq!(declarations, expected);
        assert!(!declared_declarations(source).contains_key("scoped"));
    }

    #[test]
    fn external_declaration_never_borrows_a_later_brace() {
        let source = "mod outer;\npub struct Later { field: u8 }\n";
        assert_eq!(
            declared_declarations(source).get("outer"),
            Some(&Declaration::External)
        );
        assert!(inline_module_body(source, "outer").is_none());
    }

    #[test]
    fn inline_module_scans_body_and_external_children() {
        let fixture = Fixture::new();
        fixture.write(
            "src/lib.rs",
            "pub mod alpha { pub(crate) mod child; pub struct Inline { x: u8 } }\npub mod beta;\n",
        );
        // A coincidental `src/alpha.rs` must never shadow the inline declaration.
        fixture.write("src/alpha.rs", "pub struct Coincidental;\n");
        fixture.write("src/alpha/child.rs", "use crate::beta::B;\n");
        fixture.write("src/beta.rs", "pub struct B;\n");
        let graph = analyze(&fixture.0.join("src")).unwrap();
        assert!(graph.edges.contains(&Edge {
            from: "alpha".into(),
            to: "beta".into()
        }));
        // Remove the child's dependency: the inline body has none, so the edge disappears.
        fixture.write("src/alpha/child.rs", "pub struct Child;\n");
        let graph = analyze(&fixture.0.join("src")).unwrap();
        assert!(!graph.edges.contains(&Edge {
            from: "alpha".into(),
            to: "beta".into()
        }));
    }

    #[test]
    fn inline_body_direct_paths_contribute_edges() {
        let fixture = Fixture::new();
        fixture.write(
            "src/lib.rs",
            "pub mod alpha { pub fn a() { crate::beta::B::new(); } }\npub mod beta;\n",
        );
        fixture.write("src/beta.rs", "pub struct B;\n");
        let graph = analyze(&fixture.0.join("src")).unwrap();
        assert!(graph.edges.contains(&Edge {
            from: "alpha".into(),
            to: "beta".into()
        }));
    }

    #[test]
    fn coincidental_module_file_does_not_shadow_inline_declaration() {
        let fixture = Fixture::new();
        fixture.write(
            "src/lib.rs",
            "pub mod alpha { pub struct Only; }\npub mod beta;\n",
        );
        fixture.write(
            "src/alpha.rs",
            "use crate::beta::B;\npub struct Coincidental;\n",
        );
        fixture.write("src/beta.rs", "pub struct B;\n");
        let graph = analyze(&fixture.0.join("src")).unwrap();
        assert!(!graph.edges.contains(&Edge {
            from: "alpha".into(),
            to: "beta".into()
        }));
    }

    #[test]
    fn production_capable_test_named_files_contribute_edges() {
        let fixture = Fixture::new();
        fixture.write("src/lib.rs", "pub mod alpha;\npub mod beta;\n");
        fixture.write("src/alpha.rs", "pub mod tests;\nmod test;\n");
        fixture.write("src/alpha/tests.rs", "use crate::beta::B;\n");
        fixture.write("src/alpha/test.rs", "use crate::beta::B;\n");
        fixture.write("src/beta.rs", "pub struct B;\n");
        let graph = analyze(&fixture.0.join("src")).unwrap();
        assert!(graph.edges.contains(&Edge {
            from: "alpha".into(),
            to: "beta".into()
        }));
    }

    #[test]
    fn cfg_test_only_code_does_not_contribute_edges() {
        let fixture = Fixture::new();
        fixture.write("src/lib.rs", "pub mod alpha;\npub mod beta;\n");
        fixture.write(
            "src/alpha.rs",
            "#[cfg(test)]\nmod tests { use crate::beta::B; }\npub struct A;\n",
        );
        fixture.write("src/beta.rs", "pub struct B;\n");
        let graph = analyze(&fixture.0.join("src")).unwrap();
        assert!(!graph.edges.contains(&Edge {
            from: "alpha".into(),
            to: "beta".into()
        }));
    }

    #[test]
    fn test_attribute_functions_do_not_contribute_edges() {
        let fixture = Fixture::new();
        fixture.write("src/lib.rs", "pub mod alpha;\npub mod beta;\n");
        fixture.write(
            "src/alpha.rs",
            "use crate::beta::B;\nmod helper {\npub fn f() {}\n}\n",
        );
        fixture.write(
            "src/alpha/helper.rs",
            "#[cfg(test)]\nfn gated() {\n    use crate::beta::B;\n}\n#[test]\nfn checks() {\n    use crate::beta::B;\n}\nfn plain() {\n    let _ = crate::beta::B;\n}\n",
        );
        fixture.write("src/beta.rs", "pub struct B;\n");
        let graph = analyze(&fixture.0.join("src")).unwrap();
        assert!(graph.edges.contains(&Edge {
            from: "alpha".into(),
            to: "beta".into()
        }));
        let clean = production_text(
            "use crate::beta::B;\n#[test]\nfn checks() {\n    use crate::beta::B;\n}\n",
        );
        assert!(!clean.contains("checks"));
        assert!(clean.contains("use crate::beta::B;"));
    }

    #[test]
    fn ledger_accepts_comments_and_rejects_malformed_rows() {
        let fixture = Fixture::new();
        let ledger = fixture.0.join(LEDGER_PATH);
        fs::create_dir_all(ledger.parent().unwrap()).unwrap();
        fs::write(&ledger, "# comment\n\nalpha\tbeta\t17\n").unwrap();
        assert_eq!(
            read_ledger(&ledger).unwrap(),
            vec![Debt {
                edge: Edge {
                    from: "alpha".into(),
                    to: "beta".into()
                },
                issue: 17
            }]
        );
        fs::write(&ledger, "alpha\tbeta\n").unwrap();
        assert!(read_ledger(&ledger).is_err());
        fs::write(&ledger, "alpha\tbeta\tzero\n").unwrap();
        assert!(read_ledger(&ledger).is_err());
        fs::write(&ledger, "alpha\tbeta\t17\nalpha\tbeta\t18\n").unwrap();
        assert!(read_ledger(&ledger).is_err());
    }

    #[test]
    fn gate_rejects_unowned_feedback_edges() {
        let fixture = Fixture::new();
        fixture.write(
            "src/lib.rs",
            "pub mod alpha;\npub mod beta;\npub mod leaf;\n",
        );
        fixture.write("src/alpha.rs", "use crate::beta::B;\n");
        fixture.write("src/beta.rs", "use crate::alpha::A;\n");
        fixture.write("src/leaf.rs", "pub struct Leaf;\n");
        fixture.write(LEDGER_PATH, "# empty\n");
        let ledger = fixture.0.join(LEDGER_PATH);
        let before = fs::read_to_string(&ledger).unwrap();
        let error = enforce(&fixture.0.join("src"), &[], None, false).unwrap_err();
        assert!(error.contains("1 feedback edge"), "{error}");
        assert_eq!(fs::read_to_string(&ledger).unwrap(), before);
    }

    #[test]
    fn ledger_growth_relative_to_base_requires_acceptance() {
        let fixture = Fixture::new();
        fixture.write("src/lib.rs", "pub mod alpha;\npub mod beta;\n");
        fixture.write("src/alpha.rs", "use crate::beta::B;\n");
        fixture.write("src/beta.rs", "use crate::alpha::A;\n");
        let current = vec![Debt {
            edge: Edge {
                from: "beta".into(),
                to: "alpha".into(),
            },
            issue: 17,
        }];
        let error = enforce(&fixture.0.join("src"), &current, Some(&[]), false).unwrap_err();
        assert!(error.contains("grew by 1 row"), "{error}");
        enforce(&fixture.0.join("src"), &current, Some(&[]), true).unwrap();
    }

    #[test]
    fn owner_handoff_must_match_the_ledger_exactly() {
        let ledger = vec![Debt {
            edge: Edge {
                from: "beta".into(),
                to: "alpha".into(),
            },
            issue: 17,
        }];
        check_owner_handoff(&ledger, "17\topen\n").unwrap();
        assert!(check_owner_handoff(&ledger, "").is_err());
        assert!(check_owner_handoff(&ledger, "17\tclosed\n").is_err());
        assert!(check_owner_handoff(&ledger, "17\topen\n17\topen\n").is_err());
        assert!(check_owner_handoff(&ledger, "17\topen\n23\topen\n").is_err());
    }

    #[test]
    fn base_ledger_tree_record_rejects_symlink_mode() {
        let record = format!("120000 blob 0123abcd\t{LEDGER_PATH}");
        let error = parse_tree_record(&record, "c0").unwrap_err();
        assert!(error.contains("non-blob"), "{error}");
    }

    #[test]
    fn base_ledger_tree_record_accepts_regular_blob_mode() {
        let record = format!("100644 blob 0123abcd\t{LEDGER_PATH}");
        let (path, object_id) = parse_tree_record(&record, "c0").unwrap();
        assert_eq!(path, LEDGER_PATH);
        assert_eq!(object_id, "0123abcd");
    }
    #[test]
    fn cfg_nested_not_respects_fixed_false_test_predicate() {
        let source = production_text(
            "#[cfg(not(not(test)))] use crate::removed::X;\n\
             #[cfg(not(test))] use crate::kept::X;\n\
             #[cfg(any(test, feature = \"maybe\"))] use crate::possible::X;",
        );
        assert_eq!(
            crate_paths(
                &source,
                &BTreeSet::from([
                    "removed".to_owned(),
                    "kept".to_owned(),
                    "possible".to_owned(),
                ]),
            ),
            BTreeSet::from(["kept".to_owned(), "possible".to_owned()])
        );
    }

    #[test]
    fn lexer_blanks_every_relevant_literal_and_character_escape() {
        let source = r##"
            // crate::comment::X { }
            /* crate::block::X { } */
            const A: &str = "crate::ordinary::X {";
            const B: &[u8] = b"crate::byte::X }";
            const C: &str = r"crate::raw::X {";
            const D: &[u8] = br#"crate::raw_byte::X }"#;
            const E: &CStr = c"crate::c_string::X {";
            const F: &CStr = cr#"crate::raw_c::X }"#;
            const G: char = 'x';
            const H: char = '\n';
            const I: char = '\x7b';
            const J: char = '\u{7d}';
            const K: char = 'é';
            const L: u8 = b'{';
            const M: core::ffi::c_char = c'{';
            const N: core::ffi::c_char = c'\n';
            use crate::kept::X;
        "##;
        let clean = production_text(source);
        assert_eq!(
            crate_paths(&clean, &BTreeSet::from(["kept".to_owned()])),
            BTreeSet::from(["kept".to_owned()])
        );
        assert_eq!(clean.bytes().filter(|byte| *byte == b'{').count(), 0);
        assert_eq!(clean.bytes().filter(|byte| *byte == b'}').count(), 0);
    }

    /// A malformed (unterminated) raw string whose closing quote is the final byte
    /// must be blanked wholesale instead of slicing past the end of the buffer.
    #[test]
    fn lexer_blanks_unterminated_raw_string_at_eof_without_panicking() {
        let source = "fn x() {}\nr#\"crate::beta::B";
        let clean = production_text(source);
        assert_eq!(
            crate_paths(&clean, &BTreeSet::from(["beta".to_owned()])),
            BTreeSet::new()
        );
        assert!(clean.contains("fn x()"));
    }

    #[test]
    fn scanner_handles_cfg_raw_chars_inline_modules_and_structured_groups() {
        let fixture = Fixture::new();
        fixture.write(
            "src/lib.rs",
            r#"
            mod alpha { use crate::{beta::gamma, delta}; }
            mod beta; mod gamma; mod delta;
        "#,
        );
        fixture.write(
            "src/beta.rs",
            r##"
            #[cfg(all(test, target_os = "macos"))]
            fn test_only() { use crate::gamma::G; }
            #[cfg(not(test))]
            fn maybe_production() { use crate::delta::D; }
            const RAW: &str = r#" quote " } crate::gamma::G "#;
            const CH: char = '\u{7b}';
        "##,
        );
        fixture.write("src/gamma.rs", "pub struct G;\n");
        fixture.write("src/delta.rs", "pub struct D;\n");
        let graph = analyze(&fixture.0.join("src")).unwrap();
        assert!(graph.edges.contains(&Edge {
            from: "alpha".into(),
            to: "beta".into()
        }));
        assert!(graph.edges.contains(&Edge {
            from: "alpha".into(),
            to: "delta".into()
        }));
        assert!(!graph.edges.contains(&Edge {
            from: "alpha".into(),
            to: "gamma".into()
        }));
        assert!(graph.edges.contains(&Edge {
            from: "beta".into(),
            to: "delta".into()
        }));
        assert!(!graph.edges.contains(&Edge {
            from: "beta".into(),
            to: "gamma".into()
        }));
    }

    #[test]
    fn inline_module_matching_is_exact_and_external_declarations_do_not_steal_braces() {
        let source = production_text(
            "mod foobar { use crate::wrong::X; }\n\
             mod foo; fn unrelated() { use crate::also_wrong::X; }\n\
             pub mod wanted { use crate::right::X; }",
        );
        assert_eq!(inline_module_body(&source, "foo"), None);
        assert!(inline_module_body(&source, "foobar")
            .unwrap()
            .contains("crate::wrong"));
        assert!(inline_module_body(&source, "wanted")
            .unwrap()
            .contains("crate::right"));
    }

    #[test]
    fn nested_same_named_module_does_not_shadow_the_top_level_body() {
        let fixture = Fixture::new();
        fixture.write(
            "src/lib.rs",
            "pub mod wrapper {\n    pub mod alpha { pub fn inner() { crate::gamma::G; } }\n}\n\
             pub mod alpha { pub fn outer() { crate::beta::B; } }\n\
             pub mod beta;\npub mod gamma;\n",
        );
        fixture.write("src/beta.rs", "pub struct B;\n");
        fixture.write("src/gamma.rs", "pub struct G;\n");
        let graph = analyze(&fixture.0.join("src")).unwrap();
        assert!(graph.edges.contains(&Edge {
            from: "alpha".into(),
            to: "beta".into()
        }));
        assert!(!graph.edges.contains(&Edge {
            from: "alpha".into(),
            to: "gamma".into()
        }));
    }
}
