use super::*;

/// Established authenticated-encryption/stream crates already accepted by this grader as a
/// real `[dependencies]` entry. The `rlib crate name` is the identifier a `use`
/// path is keyed on, which equals the normalized package name (dashes become underscores).
/// `name` is the `[dependencies]` TOML key (dashes as declared).
pub(super) const AEAD_ALLOW: &[(&str, &str)] = &[
    ("aes-gcm", "aes_gcm"),
    ("chacha20poly1305", "chacha20poly1305"),
    ("aead", "aead"),
    ("aes-siv", "aes_siv"),
    ("crypto_box", "crypto_box"),
    ("ring", "ring"),
    ("age", "age"),
];

/// The encryption manifest must declare an established AEAD/stream crate as a real
/// `[dependencies]` entry whose crate is then **used** in the produced source. A
/// comment or a string literal never counts.
/// Cap on how many `src/**` entries one descriptor-relative walk enumerates before stopping.
pub(super) const SRC_MAX_FILES: usize = 256;
/// Cap on how deep the descriptor-relative `src/**` walk descends.
pub(super) const SRC_MAX_DEPTH: usize = 16;
/// Cap on how many nodes a bounded same-crate call-graph scan expands.
pub(super) const GRAPH_MAX_NODES: usize = 64;
/// Bounded output for one removal-probe `cargo test --offline`.
pub(super) const PROBE_MAX_OUTPUT: usize = 512 * 1024;

/// One dependency entry extracted from the produced manifest (alias-aware): the canonical
/// package name the registry key resolves to and whether it is a local `path` dependency
/// (either its own `path` or a `workspace = true` entry that inherits one).
struct ManifestDep {
    package: String,
    is_path: bool,
}

/// Resolve one `[dependencies]`-style value to a manifest entry. A bare version string
/// is a registry dependency keyed by its own name. An inline table carries the optional
/// `package` alias and may either carry its own `path` or inherit one from
/// `[workspace.dependencies]` through `workspace = true`. Anything that is neither a string
/// nor a table fails closed.
fn dep_entry_from_value(
    key: &str,
    v: &toml::Value,
    wdeps: Option<&toml::map::Map<String, toml::Value>>,
) -> Option<ManifestDep> {
    match v {
        toml::Value::String(_) => Some(ManifestDep {
            package: key.to_string(),
            is_path: false,
        }),
        toml::Value::Table(t) => {
            let mut package: Option<&str> = t.get("package").and_then(|p| p.as_str());
            let mut is_path = t.get("path").is_some();
            if t.get("workspace").and_then(|w| w.as_bool()) == Some(true) {
                if let Some(wt) = wdeps.and_then(|m| m.get(key)).and_then(|w| w.as_table()) {
                    if wt.get("path").is_some() {
                        is_path = true;
                    }
                    if package.is_none() {
                        package = wt.get("package").and_then(|p| p.as_str());
                    }
                }
            }
            Some(ManifestDep {
                package: package.unwrap_or(key).to_string(),
                is_path,
            })
        }
        _ => None,
    }
}

/// The three dependency tables cargo reads (plus their target-specific and workspace
/// variants): normal, dev, and build dependencies.
pub(super) const DEP_TABLES: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];

/// Collect every declared dependency across `[dependencies]`, `[dev-dependencies]`,
/// `[build-dependencies]`, all `[target.*.<table>]` variants, plus the inherited
/// `[workspace.dependencies]` table itself. Only real TOML keys count: a comment-only
/// "dependency" is never one.
fn manifest_declared_deps(ws: &crate::tools::WorkspaceCap) -> Option<Vec<ManifestDep>> {
    let text = grader_file(ws, "Cargo.toml")?;
    let root: toml::Value = toml::from_str(&text).ok()?;
    let root = root.as_table()?;
    let workspace = root.get("workspace").and_then(toml::Value::as_table);
    let workspace_deps = workspace
        .and_then(|table| table.get("dependencies"))
        .and_then(toml::Value::as_table);
    let mut out = Vec::new();
    if let Some(deps) = workspace_deps {
        append_dep_table(&mut out, deps, None);
    }
    append_manifest_tables(&mut out, root, workspace_deps);
    append_target_tables(&mut out, root, workspace_deps);
    Some(out)
}

fn append_manifest_tables(
    out: &mut Vec<ManifestDep>,
    root: &toml::map::Map<String, toml::Value>,
    workspace: Option<&toml::map::Map<String, toml::Value>>,
) {
    for name in DEP_TABLES {
        if let Some(table) = root.get(name).and_then(toml::Value::as_table) {
            append_dep_table(out, table, workspace);
        }
    }
}

fn append_target_tables(
    out: &mut Vec<ManifestDep>,
    root: &toml::map::Map<String, toml::Value>,
    workspace: Option<&toml::map::Map<String, toml::Value>>,
) {
    let Some(targets) = root.get("target").and_then(toml::Value::as_table) else {
        return;
    };
    for target in targets.values().filter_map(toml::Value::as_table) {
        append_manifest_tables(out, target, workspace);
    }
}

fn append_dep_table(
    out: &mut Vec<ManifestDep>,
    table: &toml::map::Map<String, toml::Value>,
    workspace: Option<&toml::map::Map<String, toml::Value>>,
) {
    out.extend(
        table
            .iter()
            .filter_map(|(key, value)| dep_entry_from_value(key, value, workspace)),
    );
}

/// Whether the produced manifest declares any local `path` dependency, directly in any
/// dependency table or inherited through `workspace = true`.
pub(super) fn manifest_has_path_dep(ws: &crate::tools::WorkspaceCap) -> bool {
    manifest_declared_deps(ws)
        .map(|deps| deps.iter().any(|d| d.is_path))
        .unwrap_or(false)
}

/// The canonical package names (alias-resolved) of the established crypto crates the
/// produced manifest declares as **registry** (non-path) dependencies anywhere. A local
/// `path` entry to an established crate, even under an alias, is never "established".
pub(super) fn established_registry_packages(ws: &crate::tools::WorkspaceCap) -> Vec<String> {
    let Some(deps) = manifest_declared_deps(ws) else {
        return Vec::new();
    };
    AEAD_ALLOW
        .iter()
        .filter(|(name, _)| {
            deps.iter()
                .any(|d| !d.is_path && d.package.as_str() == *name)
        })
        .map(|(name, _)| (*name).to_string())
        .collect()
}
