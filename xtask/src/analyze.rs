//! Production-source discovery, per-file and per-function measurement.
//!
//! The production set is exactly the root crate's `src/` tree (`src/**/*.rs`), which
//! includes every binary under `src/bin/*`. `tests/`, `xtask/`, and `vendor/` live
//! outside that set by construction: discovery only walks `root/src`. There are no allow-lists,
//! baselines, or suppressions, and a syntax error in a production file is a fatal gate error.
//!
//! Each function (free `fn` — including `async`/`unsafe`/`extern` with a body —
//! `impl` method, trait method with a default body, and nested `fn`) is measured against its
//! own source span: effective LOC from that span, and cyclomatic/cognitive from a fresh
//! [`MetricsVisitor`] over the same subtree, so numbers never bleed between functions. A nested
//! closure is part of its containing function's span and so counts toward the containing
//! function's length.

use crate::complexity::Complexity;
use crate::complexity::MetricsVisitor;
use crate::loc;
use crate::{COGNITIVE_LIMIT, CYCLOMATIC_LIMIT, FILE_LOC_LIMIT, FUNCTION_LOC_LIMIT};
use quote::ToTokens;
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;

/// One analyzed function and its measured numbers.
#[derive(Debug)]
pub struct FunctionReport {
    /// 1-based line of the function name.
    pub line: usize,
    /// Function name, for human-addressable reporting.
    pub name: String,
    /// Effective LOC of the function's own source span.
    pub loc: usize,
    pub complexity: Complexity,
}

/// One analyzed production source file.
#[derive(Debug)]
pub struct FileReport {
    /// Path relative to the crate root, sorted.
    pub path: String,
    /// Absolute path, used for error messages.
    pub absolute: PathBuf,
    /// Effective LOC of the whole file.
    pub effective_loc: usize,
    /// Effective lines of the file (also used by the binary for reporting).
    pub lines: BTreeSet<usize>,
    pub functions: Vec<FunctionReport>,
}

/// Deterministic, sorted list of production sources: every `*.rs` under `root/src`.
pub fn find_production_sources(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut found = Vec::new();
    let source_root = root.join("src");
    let source_type = fs::symlink_metadata(&source_root)
        .map_err(|error| format!("read source directory {}: {error}", source_root.display()))?
        .file_type();
    if source_type.is_symlink() {
        return Err(format!(
            "symlink is not permitted as the production source root: {}",
            source_root.display()
        ));
    }
    if !source_type.is_dir() {
        return Err(format!(
            "production source root is not a directory: {}",
            source_root.display()
        ));
    }
    let mut stack = vec![source_root];
    while let Some(dir) = stack.pop() {
        let mut entries = fs::read_dir(&dir)
            .map_err(|error| format!("read source directory {}: {error}", dir.display()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("read source entry in {}: {error}", dir.display()))?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|error| format!("read source type {}: {error}", path.display()))?;
            if file_type.is_symlink() {
                return Err(format!(
                    "symlink is not permitted in the production source tree: {}",
                    path.display()
                ));
            } else if file_type.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                if !file_type.is_file() {
                    return Err(format!(
                        "production Rust source is not a regular file: {}",
                        path.display()
                    ));
                }
                found.push(path);
            }
        }
    }
    found.sort();
    if found.is_empty() {
        return Err(format!(
            "no production Rust sources found under {}",
            root.join("src").display()
        ));
    }
    Ok(found)
}

/// The 1-based inclusive span `[lo, hi]` covered by the item's own tokens.
fn span_lines<I: ToTokens>(item: &I) -> (usize, usize) {
    let mut lo = usize::MAX;
    let mut hi = 0usize;
    for line in loc::token_lines(&item.to_token_stream()) {
        lo = lo.min(line);
        hi = hi.max(line);
    }
    (lo, hi)
}

fn relative(root: &Path, abs: &Path) -> String {
    abs.strip_prefix(root)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| abs.display().to_string())
}

fn macro_tokens_require_expansion(tokens: proc_macro2::TokenStream) -> bool {
    let trees: Vec<_> = tokens.into_iter().collect();
    for (index, tree) in trees.iter().enumerate() {
        if let proc_macro2::TokenTree::Ident(ident) = tree {
            let name = ident.to_string();
            if matches!(name.as_str(), "include" | "macro_rules")
                && matches!(
                    trees.get(index + 1),
                    Some(proc_macro2::TokenTree::Punct(punct)) if punct.as_char() == '!'
                )
            {
                return true;
            }
        }
        match tree {
            proc_macro2::TokenTree::Ident(ident)
                if matches!(
                    ident.to_string().as_str(),
                    "fn" | "if" | "while" | "for" | "loop" | "match"
                ) =>
            {
                return true;
            }
            proc_macro2::TokenTree::Group(group)
                if macro_tokens_require_expansion(group.stream()) =>
            {
                return true;
            }
            proc_macro2::TokenTree::Punct(punct) if matches!(punct.as_char(), '&' | '|') => {
                if let Some(proc_macro2::TokenTree::Punct(next)) = trees.get(index + 1) {
                    let boolean_or_has_left_operand = match trees.get(index.wrapping_sub(1)) {
                        Some(proc_macro2::TokenTree::Ident(ident)) => {
                            !matches!(ident.to_string().as_str(), "move" | "return" | "break")
                        }
                        Some(
                            proc_macro2::TokenTree::Literal(_) | proc_macro2::TokenTree::Group(_),
                        ) => true,
                        Some(proc_macro2::TokenTree::Punct(previous)) => previous.as_char() == '?',
                        None => false,
                    };
                    if next.as_char() == punct.as_char()
                        && (punct.as_char() == '&' || boolean_or_has_left_operand)
                    {
                        return true;
                    }
                }
            }
            _ => {}
        }
    }
    false
}

fn tokens_contain_path_selection(tokens: proc_macro2::TokenStream) -> bool {
    let trees: Vec<_> = tokens.into_iter().collect();
    for (index, tree) in trees.iter().enumerate() {
        match tree {
            proc_macro2::TokenTree::Ident(ident)
                if ident == "path"
                    && matches!(
                        trees.get(index + 1),
                        Some(proc_macro2::TokenTree::Punct(punct)) if punct.as_char() == '='
                    ) =>
            {
                return true;
            }
            proc_macro2::TokenTree::Group(group)
                if tokens_contain_path_selection(group.stream()) =>
            {
                return true;
            }
            _ => {}
        }
    }

    false
}

fn tokens_contain_lint_exception(tokens: proc_macro2::TokenStream) -> bool {
    tokens.into_iter().any(|tree| match tree {
        proc_macro2::TokenTree::Ident(ident) => ident == "allow" || ident == "expect",
        proc_macro2::TokenTree::Group(group) => tokens_contain_lint_exception(group.stream()),
        _ => false,
    })
}

fn tokens_alias_include(tokens: proc_macro2::TokenStream) -> bool {
    let trees: Vec<_> = tokens.into_iter().collect();
    for (index, tree) in trees.iter().enumerate() {
        match tree {
            proc_macro2::TokenTree::Ident(ident)
                if ident == "include"
                    && matches!(
                        trees.get(index + 1),
                        Some(proc_macro2::TokenTree::Ident(next)) if next == "as"
                    ) =>
            {
                return true;
            }
            proc_macro2::TokenTree::Group(group) if tokens_alias_include(group.stream()) => {
                return true;
            }
            _ => {}
        }
    }
    false
}

#[derive(Default)]
struct MacroCheck {
    issue: Option<(usize, String)>,
}

impl<'ast> Visit<'ast> for MacroCheck {
    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        if self.issue.is_some() {
            return;
        }
        let name = node
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
            .unwrap_or_default();
        let reason = if name == "macro_rules" {
            Some("macro definitions can generate unmeasured functions or control flow")
        } else if name == "include" {
            Some("include! source is outside this file's parsed syntax tree")
        } else if macro_tokens_require_expansion(node.tokens.clone()) {
            Some("macro token stream contains syntax requiring unmeasured expansion")
        } else {
            None
        };
        if let Some(reason) = reason {
            let line = node
                .path
                .segments
                .first()
                .map(|segment| segment.ident.span().start().line)
                .unwrap_or(1);
            self.issue = Some((line, reason.to_string()));
            return;
        }
        syn::visit::visit_macro(self, node);
    }

    fn visit_attribute(&mut self, node: &'ast syn::Attribute) {
        if self.issue.is_some() {
            return;
        }
        let direct_path = node.path().is_ident("path");
        let conditional_path = node.path().is_ident("cfg_attr")
            && tokens_contain_path_selection(node.meta.to_token_stream());
        let direct_exception = node.path().is_ident("allow") || node.path().is_ident("expect");
        let conditional_exception = node.path().is_ident("cfg_attr")
            && tokens_contain_lint_exception(node.meta.to_token_stream());
        if direct_exception || conditional_exception {
            self.issue = Some((
                node.pound_token.spans[0].start().line,
                "lint allow/expect attributes are not permitted in production source".to_string(),
            ));
            return;
        }
        if direct_path || conditional_path {
            self.issue = Some((
                node.pound_token.spans[0].start().line,
                "path-selected modules can compile source outside the measured *.rs set"
                    .to_string(),
            ));
            return;
        }
        syn::visit::visit_attribute(self, node);
    }

    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        if self.issue.is_some() {
            return;
        }
        if tokens_alias_include(node.tree.to_token_stream()) {
            self.issue = Some((
                node.use_token.span.start().line,
                "renaming include! can hide source outside this file's parsed syntax tree"
                    .to_string(),
            ));
            return;
        }
        syn::visit::visit_item_use(self, node);
    }
}

/// Read, parse, and measure one production file. A syntax error is a fatal gate error.
pub fn analyze_file(root: &Path, abs: &Path) -> Result<FileReport, String> {
    let text = fs::read_to_string(abs).map_err(|e| format!("read {}: {e}", abs.display()))?;
    let file = syn::parse_file(&text).map_err(|e| format!("parse {}: {e}", abs.display()))?;
    let mut macro_check = MacroCheck::default();
    syn::visit::visit_file(&mut macro_check, &file);
    if let Some((line, reason)) = macro_check.issue {
        return Err(format!("analyze macros {}:{line}: {reason}", abs.display()));
    }

    let lines = loc::file_effective_lines(&text, &file)
        .map_err(|e| format!("analyze effective LOC {}: {e}", abs.display()))?;
    let functions = {
        let mut collect = Collect::new(&lines);
        syn::visit::visit_file(&mut collect, &file);
        collect.functions
    };

    Ok(FileReport {
        path: relative(root, abs),
        absolute: abs.to_path_buf(),
        effective_loc: lines.len(),
        lines,
        functions,
    })
}

/// Walk over a parsed file, recording every function it finds. Each function is measured with a
/// brand-new [`MetricsVisitor`] over its own subtree, so the enclosing function's metrics never
/// include a nested function's decisions.
struct Collect<'a> {
    lines: &'a BTreeSet<usize>,
    functions: Vec<FunctionReport>,
}

impl<'a> Collect<'a> {
    fn new(lines: &'a BTreeSet<usize>) -> Self {
        Collect {
            lines,
            functions: Vec::new(),
        }
    }
}

impl Collect<'_> {
    fn push(&mut self, name: String, line: usize, complexity: Complexity, lo: usize, hi: usize) {
        if lo == usize::MAX {
            return;
        }
        let loc = self.lines.range(lo..=hi).count();
        self.functions.push(FunctionReport {
            line,
            name,
            loc,
            complexity,
        });
    }
}

impl<'ast> Visit<'ast> for Collect<'_> {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        let complexity = {
            let mut m = MetricsVisitor::for_function(&node.sig.ident.to_string());
            syn::visit::visit_item_fn(&mut m, node);
            m.finish()
        };
        let (lo, hi) = span_lines(node);
        self.push(
            node.sig.ident.to_string(),
            node.sig.ident.span().start().line,
            complexity,
            lo,
            hi,
        );
        // Descend so nested functions are measured as their own units.
        syn::visit::visit_item_fn(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        let complexity = {
            let mut m = MetricsVisitor::for_function(&node.sig.ident.to_string());
            syn::visit::visit_impl_item_fn(&mut m, node);
            m.finish()
        };
        let (lo, hi) = span_lines(node);
        self.push(
            node.sig.ident.to_string(),
            node.sig.ident.span().start().line,
            complexity,
            lo,
            hi,
        );
        syn::visit::visit_impl_item_fn(self, node);
    }

    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        if node.default.is_some() {
            let complexity = {
                let mut m = MetricsVisitor::for_function(&node.sig.ident.to_string());
                syn::visit::visit_trait_item_fn(&mut m, node);
                m.finish()
            };
            let (lo, hi) = span_lines(node);
            self.push(
                node.sig.ident.to_string(),
                node.sig.ident.span().start().line,
                complexity,
                lo,
                hi,
            );
        }
        syn::visit::visit_trait_item_fn(self, node);
    }
}

/// Analyze every production source under `root/src`, in deterministic sorted order.
pub fn analyze_all(root: &Path) -> Result<Vec<FileReport>, String> {
    let mut out = Vec::new();
    for abs in find_production_sources(root)? {
        out.push(analyze_file(root, &abs)?);
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// Which fixed quality checks to execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    Loc,
    Complexity,
    All,
}

/// One fixed-threshold violation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Violation {
    pub path: String,
    pub line: usize,
    pub function: Option<String>,
    pub metric: &'static str,
    pub actual: usize,
    pub limit: usize,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: ", self.path, self.line)?;
        if let Some(function) = &self.function {
            write!(f, "{function}: ")?;
        }
        write!(f, "{} {} exceeds {}", self.metric, self.actual, self.limit)
    }
}

/// Complete deterministic analysis result.
pub struct Report {
    pub files: Vec<FileReport>,
    pub violations: Vec<Violation>,
}

impl Report {
    pub fn build(files: Vec<FileReport>, gate: Gate) -> Self {
        let mut violations = Vec::new();
        for file in &files {
            if matches!(gate, Gate::Loc | Gate::All) && file.effective_loc > FILE_LOC_LIMIT {
                violations.push(Violation {
                    path: file.path.clone(),
                    line: 1,
                    function: None,
                    metric: "effective file LOC",
                    actual: file.effective_loc,
                    limit: FILE_LOC_LIMIT,
                });
            }
            for function in &file.functions {
                if matches!(gate, Gate::Loc | Gate::All) && function.loc > FUNCTION_LOC_LIMIT {
                    violations.push(Violation {
                        path: file.path.clone(),
                        line: function.line,
                        function: Some(function.name.clone()),
                        metric: "effective function LOC",
                        actual: function.loc,
                        limit: FUNCTION_LOC_LIMIT,
                    });
                }
                if matches!(gate, Gate::Complexity | Gate::All)
                    && function.complexity.cyclomatic > CYCLOMATIC_LIMIT
                {
                    violations.push(Violation {
                        path: file.path.clone(),
                        line: function.line,
                        function: Some(function.name.clone()),
                        metric: "cyclomatic complexity",
                        actual: function.complexity.cyclomatic,
                        limit: CYCLOMATIC_LIMIT,
                    });
                }
                if matches!(gate, Gate::Complexity | Gate::All)
                    && function.complexity.cognitive > COGNITIVE_LIMIT
                {
                    violations.push(Violation {
                        path: file.path.clone(),
                        line: function.line,
                        function: Some(function.name.clone()),
                        metric: "cognitive complexity",
                        actual: function.complexity.cognitive,
                        limit: COGNITIVE_LIMIT,
                    });
                }
            }
        }
        violations.sort();
        Report { files, violations }
    }
}

/// Analyze every production file and fail after printing every violation.
pub fn run_gate(root: &Path, gate: Gate) -> Result<(), String> {
    let report = Report::build(analyze_all(root)?, gate);
    for violation in &report.violations {
        eprintln!("{violation}");
    }
    if report.violations.is_empty() {
        println!(
            "quality gate passed: {} production Rust files",
            report.files.len()
        );
        Ok(())
    } else {
        Err(format!(
            "quality gate failed: {} violation(s) in {} production Rust files",
            report.violations.len(),
            report.files.len()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn function(loc: usize, cyclomatic: usize, cognitive: usize) -> FunctionReport {
        FunctionReport {
            line: 2,
            name: "f".to_string(),
            loc,
            complexity: Complexity {
                cyclomatic,
                cognitive,
            },
        }
    }

    fn file(loc: usize, function: FunctionReport) -> FileReport {
        FileReport {
            path: "src/a.rs".to_string(),
            absolute: PathBuf::from("src/a.rs"),
            effective_loc: loc,
            lines: BTreeSet::new(),
            functions: vec![function],
        }
    }

    #[test]
    fn exact_limits_pass_and_plus_one_fails_all_metrics() {
        let pass = Report::build(
            vec![file(
                FILE_LOC_LIMIT,
                function(FUNCTION_LOC_LIMIT, CYCLOMATIC_LIMIT, COGNITIVE_LIMIT),
            )],
            Gate::All,
        );
        assert!(pass.violations.is_empty());
        let fail = Report::build(
            vec![file(
                FILE_LOC_LIMIT + 1,
                function(
                    FUNCTION_LOC_LIMIT + 1,
                    CYCLOMATIC_LIMIT + 1,
                    COGNITIVE_LIMIT + 1,
                ),
            )],
            Gate::All,
        );
        assert_eq!(fail.violations.len(), 4);
    }

    #[test]
    fn violations_are_sorted_by_path_line_and_metric() {
        let mut b = file(FILE_LOC_LIMIT + 1, function(FUNCTION_LOC_LIMIT + 1, 1, 0));
        b.path = "src/z.rs".to_string();
        let report = Report::build(
            vec![b, file(FILE_LOC_LIMIT + 1, function(1, 1, 0))],
            Gate::All,
        );
        assert_eq!(report.violations[0].path, "src/a.rs");
        assert_eq!(report.violations.last().unwrap().path, "src/z.rs");
    }

    #[test]
    fn production_discovery_only_walks_src_and_parse_failures_are_fatal() {
        let root = std::env::temp_dir().join(format!("llxprt-xtask-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src/bin")).unwrap();
        fs::create_dir_all(root.join("tests")).unwrap();
        fs::write(root.join("src/lib.rs"), "fn ok() {}\n").unwrap();
        fs::write(root.join("src/bin/b.rs"), "fn broken( {\n").unwrap();
        fs::write(root.join("tests/ignored.rs"), "fn broken( {\n").unwrap();
        let found = find_production_sources(&root).unwrap();
        assert_eq!(found.len(), 2);
        assert!(analyze_all(&root).unwrap_err().contains("parse"));
        fs::write(
            root.join("src/bin/b.rs"),
            "fn repaired() {}
",
        )
        .unwrap();
        std::os::unix::fs::symlink("lib.rs", root.join("src/link.rs")).unwrap();
        assert!(find_production_sources(&root)
            .unwrap_err()
            .contains("symlink is not permitted"));
        fs::remove_file(root.join("src/link.rs")).unwrap();
        fs::create_dir(root.join("outside")).unwrap();
        fs::write(
            root.join("outside/hidden.rs"),
            "fn hidden() {}
",
        )
        .unwrap();
        std::os::unix::fs::symlink(root.join("outside"), root.join("src/linked-dir")).unwrap();
        assert!(find_production_sources(&root)
            .unwrap_err()
            .contains("symlink is not permitted"));
        fs::remove_file(root.join("src/linked-dir")).unwrap();
        let symlink_root = root.join("symlink-root");
        fs::create_dir(&symlink_root).unwrap();
        std::os::unix::fs::symlink(root.join("src"), symlink_root.join("src")).unwrap();
        assert!(find_production_sources(&symlink_root)
            .unwrap_err()
            .contains("symlink is not permitted as the production source root"));
        fs::remove_dir_all(&symlink_root).unwrap();
        fs::remove_file(root.join("src/lib.rs")).unwrap();
        fs::remove_file(root.join("src/bin/b.rs")).unwrap();
        assert!(find_production_sources(&root)
            .unwrap_err()
            .contains("no production Rust sources"));
        fs::remove_dir_all(&root).unwrap();
        assert!(find_production_sources(&root)
            .unwrap_err()
            .contains("read source directory"));
    }

    #[test]
    fn code_bearing_macros_fail_closed() {
        let root = std::env::temp_dir().join(format!("llxprt-xtask-macros-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        let source = root.join("src/lib.rs");

        fs::write(
            &source,
            "macro_rules! hidden { () => { fn generated() { if true {} } } }\n",
        )
        .unwrap();
        assert!(analyze_file(&root, &source)
            .unwrap_err()
            .contains("macro definitions can generate unmeasured"));

        for body in [
            "if true {}",
            "a && b",
            "value()? || other",
            "outer!({ match value { _ => {} } })",
            "include!(\"generated.rs\")",
            "macro_rules! hidden { () => { 1 } }",
        ] {
            fs::write(&source, format!("fn f() {{ wrapper!({body}); }}\n")).unwrap();
            assert!(analyze_file(&root, &source)
                .unwrap_err()
                .contains("syntax requiring unmeasured expansion"));
        }

        fs::write(&source, "include!(\"generated.rs\");\n").unwrap();
        assert!(analyze_file(&root, &source)
            .unwrap_err()
            .contains("include! source"));

        for text in [
            "#[path = \"hidden.inc\"] mod hidden;\n",
            "#[cfg_attr(unix, path = \"hidden.inc\")] mod hidden;\n",
        ] {
            fs::write(&source, text).unwrap();
            assert!(analyze_file(&root, &source)
                .unwrap_err()
                .contains("path-selected modules"));
        }

        fs::write(
            &source,
            "use core::include as hidden; fn f() { hidden!(\"generated.rs\"); }\n",
        )
        .unwrap();
        assert!(analyze_file(&root, &source)
            .unwrap_err()
            .contains("renaming include!"));

        for text in [
            "#[allow(dead_code)] fn hidden() {}\n",
            "#[expect(clippy::needless_return)] fn hidden() { return; }\n",
            "#[cfg_attr(unix, allow(dead_code))] fn hidden() {}\n",
        ] {
            fs::write(&source, text).unwrap();
            assert!(analyze_file(&root, &source)
                .unwrap_err()
                .contains("lint allow/expect attributes"));
        }

        fs::write(&source, "fn f() { wrapper!(|| 1); println!(\"ok\"); }\n").unwrap();
        analyze_file(&root, &source).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
