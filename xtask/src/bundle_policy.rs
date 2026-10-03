//! Commit-derived source membership, deny policy, and vendor provenance floor.
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
    fn member_set_is_commit_not_static_allowlist() {
        let mut files = valid();
        files.push("project-plans/new/file.md".into());
        assert!(validate(&files).is_ok());
        let listing = manifest(&files);
        assert!(listing.contains("project-plans/new/\nproject-plans/new/file.md\n"));
        files.retain(|p| p != "project-plans/new/file.md");
        assert!(!manifest(&files).contains("project-plans/"));
    }
    #[test]
    fn generated_manifest_collision_is_not_silently_collapsed() {
        let listing = manifest(&[MANIFEST.into()]);
        assert_eq!(listing.lines().filter(|p| *p == MANIFEST).count(), 2);
    }
}
