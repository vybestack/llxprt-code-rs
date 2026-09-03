//! Production-module dependency graph and coupling debt gate.
//!
//! This deliberately uses a small lexer rather than a Rust parser: module dependencies are
//! determined from absolute `crate::module` paths after comments, strings, and test-only code
//! are removed. The repository's production modules are the public modules declared by
//! `src/lib.rs`; all files belonging to each of those modules are discovered recursively.

use crate::coupling_graph::{adjacency, cycle_forming_edges, strongly_connected_components};
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

pub const LEDGER_PATH: &str = "xtask/coupling-ledger.tsv";
pub const ACCEPT_ENV: &str = "LLXPRT_ACCEPT_COUPLING_LEDGER";

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct Edge {
    pub(super) from: String,
    pub(super) to: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Debt {
    edge: Edge,
    issue: u64,
}

#[derive(Debug)]
pub(super) struct Graph {
    pub(super) modules: BTreeSet<String>,
    pub(super) edges: BTreeSet<Edge>,
}

/// Analyze production dependencies and enforce the checked-in burn-down ledger.
pub fn run(workspace: &Path) -> Result<(), String> {
    run_with_paths(
        &workspace.join("src"),
        &workspace.join(LEDGER_PATH),
        env::var_os(ACCEPT_ENV).is_some(),
    )
}

fn run_with_paths(source: &Path, ledger_path: &Path, accept: bool) -> Result<(), String> {
    let graph = analyze(source)?;
    let ledger = read_ledger(ledger_path)?;
    let debt: BTreeSet<Edge> = ledger.iter().map(|item| item.edge.clone()).collect();
    let cycle_edges = cycle_forming_edges(&graph);
    let unowned: Vec<_> = cycle_edges.difference(&debt).cloned().collect();
    let retired: Vec<_> = debt.difference(&graph.edges).cloned().collect();

    report(&graph, &cycle_edges, &ledger, &unowned, &retired);

    if !unowned.is_empty() {
        return Err(format!(
            "coupling gate failed: {} cycle-forming edge(s) have no open-issue ledger entry; add an issue and run `{ACCEPT_ENV}=1 cargo xtask coupling-check` locally",
            unowned.len()
        ));
    }
    if !retired.is_empty() {
        if !accept {
            return Err(format!(
                "coupling gate failed: {} retired ledger entry(s) remain; run `{ACCEPT_ENV}=1 cargo xtask coupling-check` locally to shrink the ledger",
                retired.len()
            ));
        }
        let retained: Vec<_> = ledger
            .into_iter()
            .filter(|item| graph.edges.contains(&item.edge))
            .collect();
        write_ledger(ledger_path, &retained)?;
        println!(
            "coupling ledger accepted: {} -> {} entries",
            retained.len() + retired.len(),
            retained.len()
        );
    } else if accept {
        println!("coupling ledger unchanged: {} entries", ledger.len());
    }
    Ok(())
}

fn analyze(source: &Path) -> Result<Graph, String> {
    let lib = source.join("lib.rs");
    let lib_source = fs::read_to_string(&lib)
        .map_err(|error| format!("failed to read {}: {error}", lib.display()))?;
    let clean_lib = production_text(&lib_source);
    let modules = declared_modules(&clean_lib);
    if modules.is_empty() {
        return Err(format!("no production modules found in {}", lib.display()));
    }

    let mut edges = BTreeSet::new();
    for module in &modules {
        let files = module_files(source, module)?;
        if files.is_empty() {
            return Err(format!(
                "module `{module}` has no source file under {}",
                source.display()
            ));
        }
        for file in files {
            let raw = fs::read_to_string(&file)
                .map_err(|error| format!("failed to read {}: {error}", file.display()))?;
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

fn declared_modules(source: &str) -> BTreeSet<String> {
    let tokens = identifiers(source);
    let mut result = BTreeSet::new();
    for window in tokens.windows(3) {
        if window[0] == "pub" && window[1] == "mod" {
            result.insert(window[2].clone());
        }
    }
    result
}

fn module_files(source: &Path, module: &str) -> Result<Vec<PathBuf>, String> {
    let mut result = Vec::new();
    let file = source.join(format!("{module}.rs"));
    if file.is_file() {
        result.push(file);
    }
    let directory = source.join(module);
    if directory.is_dir() {
        collect_rs(&directory, &mut result)?;
    }
    result.sort();
    Ok(result)
}

fn collect_rs(directory: &Path, result: &mut Vec<PathBuf>) -> Result<(), String> {
    let mut entries: Vec<_> = fs::read_dir(directory)
        .map_err(|error| format!("failed to read {}: {error}", directory.display()))?
        .collect::<Result<_, _>>()
        .map_err(|error| format!("failed to read {}: {error}", directory.display()))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("failed to inspect {}: {error}", path.display()))?;
        if file_type.is_dir() {
            if entry.file_name() != "tests" {
                collect_rs(&path, result)?;
            }
        } else if file_type.is_file()
            && path.extension().is_some_and(|extension| extension == "rs")
            && !matches!(
                path.file_stem().and_then(|stem| stem.to_str()),
                Some("test" | "tests")
            )
        {
            result.push(path);
        }
    }
    Ok(())
}

fn crate_paths(source: &str, modules: &BTreeSet<String>) -> BTreeSet<String> {
    let tokens = identifiers(source);
    let mut result = BTreeSet::new();
    let mut index = 0;
    while index < tokens.len() {
        if tokens[index] == "crate" && index + 2 < tokens.len() && tokens[index + 1] == "::" {
            index += 2;
            if tokens[index] == "{" {
                let mut depth = 1usize;
                index += 1;
                while index < tokens.len() && depth > 0 {
                    match tokens[index].as_str() {
                        "{" => depth += 1,
                        "}" => depth -= 1,
                        token if depth == 1 && modules.contains(token) => {
                            result.insert(token.to_owned());
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

/// Blank comments, literals, and items immediately governed by `#[cfg(test)]`.
fn production_text(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut clean = vec![b' '; bytes.len()];
    let mut index = 0;
    let mut block_depth = 0usize;
    while index < bytes.len() {
        if block_depth > 0 {
            if bytes[index..].starts_with(b"/*") {
                block_depth += 1;
                index += 2;
            } else if bytes[index..].starts_with(b"*/") {
                block_depth -= 1;
                index += 2;
            } else {
                if bytes[index] == b'\n' {
                    clean[index] = b'\n';
                }
                index += 1;
            }
        } else if bytes[index..].starts_with(b"//") {
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
        } else if bytes[index..].starts_with(b"/*") {
            block_depth = 1;
            index += 2;
        } else if bytes[index] == b'"'
            || (bytes[index] == b'\''
                && ((index + 2 < bytes.len() && bytes[index + 2] == b'\'')
                    || (index + 3 < bytes.len()
                        && bytes[index + 1] == b'\\'
                        && bytes[index + 3] == b'\'')))
        {
            let quote = bytes[index];
            index += 1;
            while index < bytes.len() {
                if bytes[index] == b'\\' {
                    index = (index + 2).min(bytes.len());
                } else if bytes[index] == quote {
                    index += 1;
                    break;
                } else {
                    if bytes[index] == b'\n' {
                        clean[index] = b'\n';
                    }
                    index += 1;
                }
            }
        } else {
            clean[index] = bytes[index];
            index += 1;
        }
    }
    let clean = String::from_utf8(clean).expect("blanking preserves UTF-8 ASCII positions");
    remove_test_items(&clean)
}

fn remove_test_items(source: &str) -> String {
    let mut bytes = source.as_bytes().to_vec();
    let needle = b"cfg(test)";
    let mut search = 0;
    while let Some(relative) = source.as_bytes()[search..]
        .windows(needle.len())
        .position(|window| window == needle)
    {
        let found = search + relative;
        let Some(attribute_start) = source[..found].rfind("#[") else {
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

fn read_ledger(path: &Path) -> Result<Vec<Debt>, String> {
    let source = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
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
                "{}:{}: expected FROM<TAB>TO<TAB>ISSUE",
                path.display(),
                index + 1
            ));
        }
        let issue = fields[2].parse::<u64>().map_err(|_| {
            format!(
                "{}:{}: issue must be a positive number",
                path.display(),
                index + 1
            )
        })?;
        if issue == 0 || fields[0].is_empty() || fields[1].is_empty() {
            return Err(format!(
                "{}:{}: invalid ledger entry",
                path.display(),
                index + 1
            ));
        }
        let edge = Edge {
            from: fields[0].to_owned(),
            to: fields[1].to_owned(),
        };
        if !seen.insert(edge.clone()) {
            return Err(format!(
                "{}:{}: duplicate edge {} -> {}",
                path.display(),
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

fn write_ledger(path: &Path, entries: &[Debt]) -> Result<(), String> {
    let mut output = String::from(
        "# Coupling debt burn-down ledger. Entries may only be removed by the ordinary gate.\n# FROM<TAB>TO<TAB>OPEN_GITHUB_ISSUE\n",
    );
    for entry in entries {
        output.push_str(&format!(
            "{}\t{}\t{}\n",
            entry.edge.from, entry.edge.to, entry.issue
        ));
    }
    fs::write(path, output).map_err(|error| format!("failed to write {}: {error}", path.display()))
}

fn report(
    graph: &Graph,
    cycle_edges: &BTreeSet<Edge>,
    ledger: &[Debt],
    unowned: &[Edge],
    retired: &[Edge],
) {
    let adjacent = adjacency(graph);
    println!(
        "coupling: {} production modules, {} dependencies, {} cycle-forming edges, {} ledger entries",
        graph.modules.len(),
        graph.edges.len(),
        cycle_edges.len(),
        ledger.len()
    );
    println!("module dependencies:");
    for module in &graph.modules {
        let dependencies = adjacent[module].iter().cloned().collect::<Vec<_>>();
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
            "retired"
        };
        println!(
            "  {} -> {} (#{}; {state})",
            item.edge.from, item.edge.to, item.issue
        );
    }
    for edge in unowned {
        eprintln!("unowned cycle-forming edge: {} -> {}", edge.from, edge.to);
    }
    for edge in retired {
        eprintln!("retired ledger edge: {} -> {}", edge.from, edge.to);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
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
    fn new_back_edge_fails_without_ledger_growth() {
        let fixture = Fixture::new();
        fixture.write(
            "src/lib.rs",
            "pub mod alpha;\npub mod beta;\npub mod leaf;\n",
        );
        fixture.write("src/alpha.rs", "use crate::beta::B;\n");
        fixture.write("src/beta.rs", "use crate::alpha::A;\n");
        fixture.write("src/leaf.rs", "pub struct Leaf;\n");
        fixture.write(LEDGER_PATH, "# empty\n");

        let error =
            run_with_paths(&fixture.0.join("src"), &fixture.0.join(LEDGER_PATH), true).unwrap_err();
        assert!(error.contains("2 cycle-forming edge(s)"), "{error}");
        assert_eq!(
            fs::read_to_string(fixture.0.join(LEDGER_PATH)).unwrap(),
            "# empty\n"
        );
    }

    #[test]
    fn removed_debt_requires_acceptance_and_then_shrinks() {
        let fixture = Fixture::new();
        fixture.write("src/lib.rs", "pub mod alpha;\npub mod beta;\n");
        fixture.write("src/alpha.rs", "pub struct A;\n");
        fixture.write("src/beta.rs", "use crate::alpha::A;\n");
        fixture.write(
            LEDGER_PATH,
            "# Coupling debt burn-down ledger. Entries may only be removed by the ordinary gate.\n# FROM<TAB>TO<TAB>OPEN_GITHUB_ISSUE\nalpha\tbeta\t70\n",
        );

        let error = run_with_paths(&fixture.0.join("src"), &fixture.0.join(LEDGER_PATH), false)
            .unwrap_err();
        assert!(error.contains("1 retired ledger entry"), "{error}");

        run_with_paths(&fixture.0.join("src"), &fixture.0.join(LEDGER_PATH), true).unwrap();
        let ledger = read_ledger(&fixture.0.join(LEDGER_PATH)).unwrap();
        assert!(ledger.is_empty());
    }

    #[test]
    fn ignores_test_only_dependencies_and_finds_nested_module_files() {
        let fixture = Fixture::new();
        fixture.write("src/lib.rs", "pub mod alpha;\npub mod beta;\n");
        fixture.write(
            "src/alpha.rs",
            "#[cfg(test)]\nmod tests { use crate::beta::B; }\npub struct A;\n",
        );
        fixture.write("src/alpha/nested.rs", "use crate::beta::B;\n");
        fixture.write("src/beta.rs", "pub struct B;\n");
        let graph = analyze(&fixture.0.join("src")).unwrap();
        assert!(graph.edges.contains(&Edge {
            from: "alpha".into(),
            to: "beta".into()
        }));
        fixture.write("src/alpha/nested.rs", "pub struct Nested;\n");
        let graph = analyze(&fixture.0.join("src")).unwrap();
        assert!(graph.edges.is_empty());
    }
}
