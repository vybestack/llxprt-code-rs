//! Commit-derived release selection, deny policy, and vendor provenance floor.
use crate::release_support::{text, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

pub const MANIFEST: &str = "THIRD_PARTY_LICENSES/source-bundle.txt";
pub const DIGESTS: &str = "THIRD_PARTY_LICENSES/source-bundle.sha256";
pub const FLOOR: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "LICENSE",
    "README.md",
    "PATCHES.md",
    ".gitignore",
    ".gitattributes",
    ".cargo/config.toml",
    "scripts/build-source-bundle.sh",
    "scripts/verify-source-bundle.sh",
    "scripts/source-bundle-validate.py",
    "scripts/source_bundle_policy.py",
    "scripts/source-bundle-policy.txt",
    "scripts/materialize-git-tree.py",
    "scripts/verify-source-inputs-git.sh",
    "xtask/Cargo.toml",
    "xtask/Cargo.lock",
    "xtask/src/main.rs",
    "xtask/src/release.rs",
];

pub fn commit(root: &Path) -> Result<String> {
    text(
        Command::new("git")
            .current_dir(root)
            .args(["rev-parse", "--verify", "HEAD^{commit}"]),
    )
    .map(|s| s.trim().to_string())
    .map_err(|e| format!("a committed Git HEAD is required for source-bundle publication: {e}"))
}

pub fn members(root: &Path, commit: &str) -> Result<Vec<String>> {
    let tree = text(
        Command::new("git")
            .current_dir(root)
            .args(["ls-tree", "-rz", commit]),
    )?;
    tree.split('\0')
        .filter(|s| !s.is_empty())
        .map(|entry| {
            let (header, path) = entry.split_once('\t').ok_or("invalid Git tree record")?;
            if !(header.starts_with("100644 blob ") || header.starts_with("100755 blob ")) {
                return Err(format!(
                    "non-regular tree entries are not permitted in source-bundle inputs: {path}"
                ));
            }
            if path.bytes().any(|b| matches!(b, 0 | 9 | 10 | 13 | 127)) {
                return Err("control characters are not permitted in source-bundle paths".into());
            }
            Ok(path.to_owned())
        })
        .collect()
}

/// Shared with the pre-extraction validator through the same checked-in rule file.
/// No dependency on internal designs or proposals is compiled into release tooling.
const POLICY: &str = include_str!("../../scripts/source-bundle-policy.txt");

fn matches(path: &str, pattern: &str) -> bool {
    if let Some(root) = pattern.strip_suffix("/**") {
        path == root || path.strip_prefix(root).is_some_and(|p| p.starts_with('/'))
    } else if let Some((before, after)) = pattern.split_once('*') {
        !path.contains('/') && path.starts_with(before) && path.ends_with(after)
    } else {
        path == pattern
    }
}

pub fn selected(path: &str) -> bool {
    let path = path.trim_end_matches('/');
    let mut included = false;
    for line in POLICY
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let (action, pattern) = line.split_once(' ').expect("checked release policy syntax");
        if matches(path, pattern) {
            match action {
                "exclude" => return false,
                "include" => included = true,
                _ => panic!("invalid release policy action: {action}"),
            }
        }
    }
    included
}

pub fn release_members(root: &Path, commit: &str) -> Result<Vec<String>> {
    Ok(members(root, commit)?
        .into_iter()
        .filter(|p| selected(p))
        .collect())
}

pub fn manifest(files: &[String]) -> String {
    let mut entries = files.to_vec();
    let mut directories = BTreeSet::new();
    for path in files {
        for (index, _) in path.match_indices('/') {
            directories.insert(format!("{}/", &path[..index]));
        }
    }
    entries.extend(directories);
    entries.extend([DIGESTS.to_owned(), MANIFEST.to_owned()]);
    // Do not deduplicate files: a committed generated manifest must fail validation.
    entries.sort();
    entries.join("\n") + "\n"
}

pub fn forbidden(path: &str) -> bool {
    let parts: Vec<_> = path.trim_end_matches('/').split('/').collect();
    let last = parts.last().copied().unwrap_or("");
    if matches!(last, ".cargo-ok" | ".rustc_info.json") {
        return true;
    }
    if path.starts_with("registry-vendor/") {
        return false;
    }
    parts.iter().any(|s| {
        matches!(
            *s,
            ".git" | "target" | "dist" | "llxprt-parity-out" | "__pycache__"
        )
    }) || last == ".DS_Store"
        || [".log", ".tmp", ".temp", ".pyc"]
            .iter()
            .any(|s| last.ends_with(s))
}

pub fn validate(files: &[String]) -> Result {
    if let Some(path) = files.iter().find(|p| !selected(p)) {
        return Err(format!("path excluded by source release policy: {path}"));
    }
    if let Some(path) = files.iter().find(|p| forbidden(p)) {
        return Err(format!(
            "forbidden paths are not permitted in source-bundle inputs: {path}"
        ));
    }
    for member in FLOOR {
        if !files.iter().any(|p| p == member) {
            return Err(format!(
                "source bundle is missing load-bearing member: {member}"
            ));
        }
    }
    for prefix in ["src/", "registry-vendor/", "THIRD_PARTY_LICENSES/"] {
        if !files.iter().any(|p| p.starts_with(prefix)) {
            return Err(format!(
                "source bundle is missing load-bearing tree: {prefix}"
            ));
        }
    }
    if !files
        .iter()
        .any(|p| p.starts_with("SERDES-AI-") && p.ends_with(".patch") && !p.contains('/'))
    {
        return Err("source bundle is missing the SerdesAI patch input".into());
    }
    vendor_pairs(files)
}

fn vendor_pairs(files: &[String]) -> Result {
    let mut crates: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for path in files {
        if let Some(path) = path.strip_prefix("vendor/") {
            if let Some((name, member)) = path.split_once('/') {
                crates.entry(name).or_default().insert(member);
            }
        }
    }
    for (name, members) in crates {
        if !members.contains("Cargo.toml") {
            continue;
        }
        for required in ["Cargo.lock", "src/"] {
            if !members.iter().any(|p| {
                if required.ends_with('/') {
                    p.starts_with(required)
                } else {
                    *p == required
                }
            }) {
                return Err(format!(
                    "vendored crates are missing provenance members: vendor/{name}/{required}"
                ));
            }
        }
        if members.contains("Cargo.toml.orig") != members.contains(".cargo_vcs_info.json") {
            return Err(format!("vendored crates are missing provenance members: vendor/{name}/Cargo.toml.orig+.cargo_vcs_info.json"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn valid() -> Vec<String> {
        FLOOR
            .iter()
            .copied()
            .chain([
                "src/lib.rs",
                "registry-vendor/a/file.tmp",
                "THIRD_PARTY_LICENSES/README.md",
                "SERDES-AI-0.2.6.patch",
                "vendor/a/Cargo.toml",
                "vendor/a/Cargo.lock",
                "vendor/a/src/lib.rs",
            ])
            .map(str::to_owned)
            .collect()
    }
    #[test]
    fn every_floor_member_is_load_bearing() {
        for member in FLOOR {
            let mut files = valid();
            files.retain(|p| p != member);
            assert!(validate(&files).unwrap_err().contains(member));
        }
        assert!(validate(&valid()).is_ok());
    }
    #[test]
    fn deny_rules_and_registry_exception() {
        for path in [
            "src/.git/config",
            "src/target/a",
            "dist/a",
            "a/llxprt-parity-out/a",
            "a/__pycache__/a",
            "a/a.log",
            "a/a.tmp",
            "a/a.temp",
            "a/a.pyc",
            "a/.DS_Store",
            "registry-vendor/a/.cargo-ok",
            "registry-vendor/a/.rustc_info.json",
        ] {
            assert!(forbidden(path), "{path}");
        }
        for path in [
            "registry-vendor/a/dist/file.tmp",
            "src/log.rs",
            "a/targeted/a",
        ] {
            assert!(!forbidden(path), "{path}");
        }
    }
    #[test]
    fn vendor_pairs_are_both_or_neither() {
        let mut files = valid();
        files.push("vendor/a/Cargo.toml.orig".into());
        assert!(validate(&files).is_err());
        files.push("vendor/a/.cargo_vcs_info.json".into());
        assert!(validate(&files).is_ok());
        files.retain(|p| p != "vendor/a/Cargo.toml.orig");
        assert!(validate(&files).is_err());
        files.retain(|p| p != "vendor/a/.cargo_vcs_info.json");
        files.retain(|p| p != "vendor/a/Cargo.lock");
        assert!(validate(&files).is_err());
    }
    #[test]
    fn recursive_release_policy_is_deliberate_not_all_tracked_files() {
        for path in [
            "src/new/module.rs",
            "tests/new/case.rs",
            "docs/new/guide.md",
        ] {
            assert!(selected(path), "{path}");
            let mut files = valid();
            files.push(path.into());
            assert!(validate(&files).is_ok());
            assert!(manifest(&files).contains(path));
        }
        for path in [
            "project-plans/new/file.md",
            "design-docs/new/file.md",
            ".agents/skill.md",
            "UNLISTED.md",
            "unknown/src/lib.rs",
        ] {
            assert!(!selected(path), "{path}");
            let mut files = valid();
            files.push(path.into());
            assert!(validate(&files)
                .unwrap_err()
                .contains("excluded by source release policy"));
        }
        assert!(selected("SERDES-AI-9.0.patch"));
        assert!(!selected("SERDES-AI-a/b.patch"));
        assert!(!selected("docs-other/file.md"));
    }
    #[test]
    fn release_policy_syntax_is_unambiguous() {
        for line in POLICY
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
        {
            let fields: Vec<_> = line.split_whitespace().collect();
            assert_eq!(fields.len(), 2);
            assert!(matches!(fields[0], "include" | "exclude"));
            let pattern = fields[1];
            let stars = pattern.chars().filter(|c| *c == '*').count();
            assert!(
                stars == 0
                    || (stars == 2 && pattern.ends_with("/**"))
                    || (stars == 1 && !pattern.contains('/'))
            );
        }
    }
    #[test]
    fn generated_manifest_collision_is_not_silently_collapsed() {
        let listing = manifest(&[MANIFEST.into()]);
        assert_eq!(listing.lines().filter(|p| *p == MANIFEST).count(), 2);
    }
}
