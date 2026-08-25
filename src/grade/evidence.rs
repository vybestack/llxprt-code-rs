use super::*;

/// Same-crate evidence: parsed source ASTs, unique top-level function locations, direct crate-root
/// exports, and identifiers that resolve to acknowledged crypto crates.
pub(super) struct CrateEvidence {
    pub(super) files: Vec<syn::File>,
    fns: HashMap<String, usize>,
    pub(super) fn_names: HashSet<String>,
    root_exports: HashSet<String>,
    pub(super) crypto: HashSet<String>,
    pub(super) local_methods: HashSet<String>,
}

impl CrateEvidence {
    /// Locate a uniquely named top-level `fn` item in its recorded source file.
    pub(super) fn fn_item(&self, name: &str) -> Option<&syn::ItemFn> {
        let fi = self.fns.get(name)?;
        let file = self.files.get(*fi)?;
        for item in &file.items {
            if let syn::Item::Fn(f) = item {
                if f.sig.ident == name {
                    return Some(f);
                }
            }
        }
        None
    }

    /// Whether the crate root directly exports the named `fn`.
    pub(super) fn is_exported(&self, name: &str) -> bool {
        self.root_exports.contains(name)
    }
}

/// Parse every collected `src/**/*.rs` file. Conditional/item-generating syntax, duplicate
/// top-level function names, parse failures, and an absent library root all fail closed.
pub(super) fn build_crate_evidence(
    ws: &crate::tools::WorkspaceCap,
    roots: &HashSet<String>,
) -> Option<CrateEvidence> {
    let sources = collect_sources(ws)?;
    if !has_default_library_target(&sources)
        || !sources.iter().any(|s| s.rel == Path::new("src/lib.rs"))
    {
        return None;
    }
    let mut crypto = roots.clone();
    let mut local_methods = HashSet::new();
    let mut parsed_files = Vec::new();
    let mut lib_file = None;
    for src in sources
        .iter()
        .filter(|src| src.rel.extension().is_some_and(|ext| ext == "rs"))
    {
        let file: syn::File = syn::parse_str(&src.text).ok()?;
        if flow::has_unsupported_item_syntax(&file) {
            return None;
        }
        collect_crypto_imports(&file, &mut crypto);
        if src.rel == Path::new("src/lib.rs") {
            lib_file = Some(parsed_files.len());
        }
        local_methods.extend(flow::local_method_names(&file));
        parsed_files.push(file);
    }
    finish_evidence(parsed_files, lib_file?, crypto, local_methods)
}

fn has_default_library_target(sources: &[SourceFile]) -> bool {
    let Some(manifest) = sources
        .iter()
        .find(|source| source.rel == Path::new("Cargo.toml"))
        .and_then(|source| source.text.parse::<toml::Value>().ok())
    else {
        return false;
    };
    if manifest
        .get("package")
        .and_then(|package| package.get("autolib"))
        .and_then(toml::Value::as_bool)
        == Some(false)
    {
        return false;
    }
    manifest
        .get("lib")
        .and_then(toml::Value::as_table)
        .and_then(|lib| lib.get("path"))
        .is_none_or(|path| path.as_str() == Some("src/lib.rs"))
}

fn collect_crypto_imports(file: &syn::File, crypto: &mut HashSet<String>) {
    for item in &file.items {
        if let syn::Item::Use(u) = item {
            let mut imported = HashSet::new();
            collect_use_tree(&u.tree, &mut Vec::new(), crypto, &mut imported);
            crypto.extend(imported);
        }
    }
}

fn finish_evidence(
    files: Vec<syn::File>,
    lib_file: usize,
    crypto: HashSet<String>,
    local_methods: HashSet<String>,
) -> Option<CrateEvidence> {
    let mut fns = HashMap::new();
    let mut root_exports = HashSet::new();
    for (file_index, file) in files.iter().enumerate() {
        for item in &file.items {
            let syn::Item::Fn(function) = item else {
                continue;
            };
            let name = function.sig.ident.to_string();
            if fns.contains_key(&name) || fns.len() >= GRAPH_MAX_NODES {
                return None;
            }
            if file_index == lib_file && matches!(function.vis, syn::Visibility::Public(..)) {
                root_exports.insert(name.clone());
            }
            fns.insert(name, file_index);
        }
    }
    let fn_names = fns.keys().cloned().collect();
    Some(CrateEvidence {
        files,
        fns,
        fn_names,
        root_exports,
        crypto,
        local_methods,
    })
}
