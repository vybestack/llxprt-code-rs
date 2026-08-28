//! Phase 0 repository self-tests for the profile-fixture and
//! compatibility-inventory artifacts (issue 1).
//!
//! These tests are independent: they read only the checked-in
//! `tests/fixtures/profile-compatibility-inventory.json`, the redacted
//! `tests/fixtures/profiles/` fixtures, and `docs/profile-compatibility.md`.
//! They never read the sibling checkout or the installed profiles directory, and
//! they never implement Phase 1 parsing. They fail if the inventory is
//! internally inconsistent or if any tracked fixture field is unclassified or
//! multiply owned.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Artifact {
    counts: Counts,
    inventory: Inventory,
    classifications: Vec<Classification>,
    profiles: Profiles,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Counts {
    profile_json_files: usize,
    in_scope_open_ai: usize,
    in_scope_codex: usize,
    installed_in_scope: usize,
    load_balancer: usize,
    unsupported_providers: usize,
    inventory_total_entries: usize,
    inventory_distinct_keys: usize,
    duplicate_groups: usize,
    duplicate_extra_entries: usize,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Inventory {
    total_entries: usize,
    distinct_keys: usize,
    duplicate_groups: Vec<DuplicateGroup>,
    alias_normalization_rules: BTreeMap<String, String>,
    entries: Vec<InventoryEntry>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DuplicateGroup {
    key: String,
    count: usize,
    files: Vec<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct InventoryEntry {
    key: String,
    #[serde(rename = "type")]
    key_type: Option<String>,
    source: String,
    aliases: Vec<String>,
    application_paths: Vec<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Classification {
    key: String,
    classification: String,
    owner: String,
    note: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Profiles {
    installed_in_scope: Vec<InstalledRow>,
    load_balancer: Vec<LoadBalancerRow>,
    unsupported_providers: Vec<UnsupportedRow>,
    synthetic: Vec<SyntheticRow>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstalledRow {
    file: String,
    provider: String,
    scope: String,
    top_level: Vec<String>,
    structure: serde_json::Value,
    paths: Vec<String>,
    expected_disposition: serde_json::Value,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoadBalancerRow {
    file: String,
    provider: String,
    reason: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct UnsupportedRow {
    provider: String,
    files: Vec<String>,
    reason: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyntheticRow {
    file: String,
    kind: String,
    expected_outcome: String,
}

fn test_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn artifact() -> Artifact {
    let raw =
        fs::read_to_string(test_root().join("tests/fixtures/profile-compatibility-inventory.json"))
            .expect("checked-in profile-compatibility-inventory.json must exist");
    serde_json::from_str(&raw).expect("artifact must be valid JSON")
}

fn fixture_paths() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(test_root().join("tests/fixtures/profiles"))
        .expect("tests/fixtures/profiles must exist")
        .map(|e| e.expect("readable entry").path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    out.sort();
    out
}

/// Collect every leaf field path (dot-joined object keys, container retained).
fn collect_paths(value: &serde_json::Value) -> BTreeSet<String> {
    fn walk(prefix: &str, value: &serde_json::Value, out: &mut BTreeSet<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (k, v) in map {
                    let next = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{prefix}.{k}")
                    };
                    walk(&next, v, out);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    walk(prefix, item, out);
                }
            }
            _ => {
                out.insert(if prefix.is_empty() {
                    "<scalar>".to_string()
                } else {
                    prefix.to_string()
                });
            }
        }
    }
    let mut out = BTreeSet::new();
    walk("", value, &mut out);
    out
}

fn typed_paths(value: &serde_json::Value) -> BTreeSet<String> {
    collect_paths(value)
}

/// Resolve a fixture leaf path to its classification key using the plan's
/// normalized-path rule: a path retains its source container (`modelParams` or
/// `ephemeralSettings`) followed by a dot and the literal nested key; long
/// registered dotted keys such as `reasoning.enabled` retain those dots, and
/// nested containers such as `modelParams.provider` / `modelParams.chat_template_kwargs`
/// classify by their longest registered dotted prefix. Any path with no resolution
/// is unclassified.
fn normalize_path(path: &str, classifications: &[Classification]) -> Vec<String> {
    let exact: Vec<String> = classifications
        .iter()
        .filter(|c| c.key == path)
        .map(|c| c.key.clone())
        .collect();
    if !exact.is_empty() {
        return exact;
    }
    if let Some(stripped) = path.strip_prefix("modelParams.") {
        let mut prefixed: Vec<String> = classifications
            .iter()
            .filter(|c| c.key.contains('.') && path.starts_with(&c.key))
            .map(|c| c.key.clone())
            .collect();
        if !prefixed.is_empty() {
            prefixed.sort_by_key(|k| std::cmp::Reverse(k.len()));
            return prefixed;
        }
        if classifications.iter().any(|c| c.key == stripped) {
            return vec![stripped.to_string()];
        }
    }
    if let Some(stripped) = path.strip_prefix("ephemeralSettings.") {
        if classifications.iter().any(|c| c.key == stripped) {
            return vec![stripped.to_string()];
        }
    }
    Vec::new()
}

fn assert_all_fields_classified_exactly_once(artifact: &Artifact) {
    let owner_counts: BTreeMap<&str, BTreeSet<(&str, &str)>> = artifact
        .classifications
        .iter()
        .fold(BTreeMap::new(), |mut acc, c| {
            acc.entry(c.key.as_str())
                .or_default()
                .insert((c.classification.as_str(), c.owner.as_str()));
            acc
        });
    for (key, owners) in &owner_counts {
        assert_eq!(
            owners.len(),
            1,
            "key {key:?} has more than one classification/owner: {owners:?}"
        );
    }

    let mut violations = Vec::new();
    for path in &fixture_paths() {
        let text = fs::read_to_string(path).expect("fixture readable");
        let value: serde_json::Value =
            serde_json::from_str(&text).expect("fixture must be valid JSON");
        for leaf in typed_paths(&value) {
            let resolved = normalize_path(&leaf, &artifact.classifications);
            if resolved.is_empty() {
                violations.push(format!("{} :: {}", path.display(), leaf));
            } else if resolved.len() != 1 {
                panic!(
                    "fixture field {leaf:?} in {} resolves to multiple owners: {resolved:?}",
                    path.display()
                );
            }
        }
    }
    assert!(
        violations.is_empty(),
        "fixture fields with no classification (file :: leaf):\n{}",
        violations.join("\n")
    );
}

#[test]
fn inventory_counts_are_exact() {
    let a = artifact();
    // counts.profile_json_files counts the installed profiles directory (the
    // installed sources) plus the checked-in codex fixture rows; the three
    // installed load-balancer shapes are inventoried as rows but the fixtures
    // directory carries 65 redacted files.
    assert_eq!(a.counts.profile_json_files, 62);
    assert_eq!(a.counts.installed_in_scope, 39);
    assert_eq!(a.inventory.total_entries, 124);
    assert_eq!(a.inventory.distinct_keys, 123);
    assert_eq!(
        a.counts.in_scope_open_ai, 38,
        "installed provider=openai profiles"
    );
    assert_eq!(
        a.counts.in_scope_codex, 1,
        "installed provider=codex profile"
    );
    assert_eq!(a.counts.load_balancer, 3);
    assert_eq!(a.counts.unsupported_providers, 6);
    assert_eq!(a.counts.inventory_total_entries, 124);
    assert_eq!(a.counts.inventory_distinct_keys, 123);
    assert_eq!(a.counts.duplicate_groups, 1);
    assert_eq!(a.counts.duplicate_extra_entries, 1);

    assert_eq!(a.profiles.installed_in_scope.len(), 39);
    let openai_count = a
        .profiles
        .installed_in_scope
        .iter()
        .filter(|r| r.provider == "openai")
        .count();
    assert_eq!(openai_count, 38);
    let codex = a
        .profiles
        .installed_in_scope
        .iter()
        .find(|r| r.provider == "codex")
        .expect("gpt56solhigh.json row");
    assert_eq!(codex.file, "gpt56solhigh.json");
    assert_eq!(codex.scope, "codex");
}

#[test]
fn inventory_is_internally_consistent() {
    let a = artifact();
    let entries = &a.inventory.entries;
    assert_eq!(entries.len(), a.counts.inventory_total_entries);
    let distinct: BTreeSet<&str> = entries.iter().map(|e| e.key.as_str()).collect();
    assert_eq!(distinct.len(), a.counts.inventory_distinct_keys);

    let mut by_key: BTreeMap<&str, usize> = BTreeMap::new();
    for e in entries {
        *by_key.entry(e.key.as_str()).or_insert(0) += 1;
    }
    let dup_extra: usize = by_key.values().map(|n| n.saturating_sub(1)).sum();
    assert_eq!(dup_extra, a.counts.duplicate_extra_entries);
    assert_eq!(
        a.inventory.duplicate_groups.len(),
        1,
        "auth.noBrowser is the only duplicate"
    );
    assert_eq!(a.inventory.duplicate_groups[0].key, "auth.noBrowser");
    assert_eq!(a.inventory.duplicate_groups[0].count, 2);
    assert_eq!(a.inventory.duplicate_groups[0].files.len(), 2);

    for entry in entries {
        assert!(entry.key_type.is_some(), "inventory entry type is explicit");
        assert!(
            !entry.source.is_empty(),
            "inventory entry source is explicit"
        );
        assert!(
            !entry.application_paths.is_empty(),
            "inventory entry application path is explicit"
        );
    }
    assert!(
        a.classifications.iter().all(|row| !row.note.is_empty()),
        "every classification explains its policy"
    );

    // Every alias resolves to exactly one canonical owner and the rules agree.
    let canonical = entries
        .iter()
        .map(|e| e.key.as_str())
        .collect::<BTreeSet<_>>();
    for (alias, target) in &a.inventory.alias_normalization_rules {
        assert!(
            canonical.contains(target.as_str()),
            "alias rule {alias:?} -> {target:?} references a known canonical key"
        );
    }
    let mut alias_targets: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for e in entries {
        for alias in &e.aliases {
            alias_targets
                .entry(alias.as_str())
                .or_default()
                .push(&e.key);
        }
    }
    for (alias, owners) in &alias_targets {
        assert_eq!(
            owners.len(),
            1,
            "alias {alias:?} belongs to exactly one canical key (got {owners:?})"
        );
    }
}

#[test]
fn every_fixture_field_has_exactly_one_classification() {
    assert_all_fields_classified_exactly_once(&artifact());
}

#[test]
fn load_balancer_and_unsupported_rows_are_present() {
    let a = artifact();
    let lb_files: Vec<&str> = a
        .profiles
        .load_balancer
        .iter()
        .map(|r| r.file.as_str())
        .collect();
    assert_eq!(
        lb_files,
        vec!["glm.json", "gptfirst.json", "opusfirst.json"]
    );
    for row in &a.profiles.load_balancer {
        assert!(
            row.provider.is_empty(),
            "load-balancer shapes have no provider key"
        );
        assert!(row.reason.contains("unsupported-load-balancing"));
    }
    assert_eq!(a.profiles.unsupported_providers.len(), 6);
    for row in &a.profiles.unsupported_providers {
        assert!(!row.provider.is_empty());
        assert!(!row.files.is_empty());
        assert!(row.reason.contains("provider-resolution"));
    }
    assert!(
        a.profiles
            .synthetic
            .iter()
            .all(|row| !row.expected_outcome.is_empty()),
        "every synthetic row has a disposition"
    );
    assert!(a
        .profiles
        .synthetic
        .iter()
        .any(|s| s.kind == "top-level-auth"));
    assert!(a
        .profiles
        .synthetic
        .iter()
        .any(|s| s.file.starts_with("loadbalancer.")));
    assert!(a
        .profiles
        .synthetic
        .iter()
        .any(|s| s.kind == "unsupported-provider-shape"));
}

#[test]
fn fixture_directory_matches_inventory_rows() {
    let a = artifact();
    let installed: BTreeSet<&str> = a
        .profiles
        .installed_in_scope
        .iter()
        .map(|r| r.file.as_str())
        .collect();
    let synthetic: BTreeSet<&str> = a
        .profiles
        .synthetic
        .iter()
        .map(|r| r.file.as_str())
        .collect();
    let on_disk: BTreeSet<String> = fixture_paths()
        .iter()
        .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(String::from))
        .filter(|name| name != "openai-responses-live.json")
        .collect();
    assert_eq!(on_disk.len(), 65);
    assert!(installed.iter().all(|f| on_disk.contains(*f)));
    assert!(synthetic.iter().all(|f| on_disk.contains(*f)));
    assert_eq!(installed.iter().collect::<BTreeSet<_>>().len(), 39);
    assert_eq!(synthetic.iter().collect::<BTreeSet<_>>().len(), 26);
}

#[test]
fn codex_fixture_preserves_structure() {
    let a = artifact();
    let codex = a
        .profiles
        .installed_in_scope
        .iter()
        .find(|r| r.file == "gpt56solhigh.json")
        .expect("codex row");
    let text = fs::read_to_string(test_root().join("tests/fixtures/profiles/gpt56solhigh.json"))
        .expect("codex fixture");
    let value: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
    let obj = value.as_object().expect("object");
    assert!(
        codex.structure.is_object(),
        "codex structure manifest is typed"
    );
    assert!(
        codex.expected_disposition.is_object(),
        "codex disposition manifest is structured"
    );
    assert_eq!(obj.get("provider").and_then(|v| v.as_str()), Some("codex"));
    assert_eq!(obj.get("version").and_then(|v| v.as_i64()), Some(1));
    let disk_paths = typed_paths(&value);
    for path in &codex.paths {
        assert!(
            disk_paths.contains(path),
            "codex structure path {path:?} present"
        );
    }
    for top in &codex.top_level {
        assert!(obj.contains_key(top), "codex top-level key {top:?} present");
    }
}

#[test]
fn documentation_is_present_and_consistent() {
    let doc = fs::read_to_string(test_root().join("docs/profile-compatibility.md"))
        .expect("docs/profile-compatibility.md exists");
    assert!(doc.contains("124"), "docs mention the derived entry count");
    assert!(doc.contains("123"), "docs mention the distinct key count");
    assert!(
        doc.contains("tests/profile_compatibility.rs"),
        "docs point at the repository self-tests"
    );
    assert!(doc.contains("generate-profile-compatibility-inventory.mjs"));
    let a = artifact();
    let c = &a.counts;
    assert!(
        doc.contains(&format!(
            "({} openai + {} codex)",
            c.in_scope_open_ai, c.in_scope_codex
        )) || doc.contains("38` provider"),
        "docs mention the in-scope 38 openai + 1 codex partition"
    );
}
