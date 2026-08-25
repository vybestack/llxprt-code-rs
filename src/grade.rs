//! Grading for the parity scenarios. After the agent finishes, the grader re-runs the
//! real test/build commands against the produced workspace (so it proves the artifact
//! actually builds and tests green) and scores protocol, tool-use, build/test, and
//! structure independently.
//!
//! A scenario is considered green only when every required file exists, every verification
//! command exits 0, and a matching hidden grader finds the required behavior. Build or
//! structural green is evidence, not a claim from the model.
//!
//! Required and hidden-grader files are read through descriptor-relative `openat`
//! handles with `O_NOFOLLOW` and a `cap + 1` bounded read, so a symlink or an
//! oversized file is rejected before any full allocation. The Pong/Flappy hidden graders
//! run a grader-authored Python probe against the produced module (artifact-authored tests are
//! never treated as hidden evidence). The encryption hidden graders parse `[dependencies]`
//! with a real TOML parser (comments are not dependency keys) and require actual crate
//! identifier usage in the produced Rust sources: comments and string literals are stripped
//! before scanning, so a crate named only in prose cannot satisfy the dependency grader.
//! The consumer grader builds a temporary external Rust consumer that depends on the produced
//! crate by path and runs `cargo test --offline` with nonidentity, roundtrip,
//! wrong-password, tamper, double-encrypt-distinct, and bounded-ciphertext-overhead
//! assertions.

use crate::harness::Inventory;
use crate::process::{self, CmdSpec};
use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

fn workspace_fd(workspace: &crate::tools::WorkspaceCap) -> i32 {
    use std::os::fd::AsRawFd;
    workspace.root_dir().as_raw_fd()
}

/// Cap on how many bytes one hidden-grader file probe ever reads.
const GRADER_MAX_BYTES: usize = 4 * 1024 * 1024;
/// Cap on how many bytes one hidden-grader source scan reads (a single `.rs` file is
/// probed at `cap + 1`, so an oversized file is rejected before allocation).
const SCAN_MAX_BYTES: usize = 2 * 1024 * 1024;
/// Bounded budget for one `cargo test --offline` run of the hidden consumer.
const CONSUMER_MAX_OUTPUT: usize = 512 * 1024;
/// Bounded timeout for one `cargo test --offline` run of the hidden consumer.
const CONSUMER_TIMEOUT: Duration = Duration::from_secs(300);
/// The hidden consumer requires enough ciphertext overhead to carry a nonce and authentication
/// tag. Password salts are recommended, but some accepted APIs derive a key outside the payload.
const MIN_OVERHEAD: usize = 24;
/// The two-pass distinct ciphertext check encrypts identical plaintext/password this many times
/// in a row (must all differ; identical bytes means a fixed nonce/deterministic codec).
const DISTINCT_ENCRYPT_PASSES: usize = 4;

/// What each scenario must contain structurally.
pub fn required_files(scenario: &str) -> &'static [&'static str] {
    match scenario {
        "starter" => &["math_utils.py", "test_math_utils.py"],
        "pong" => &["pong_logic.py", "pong.py", "test_pong.py"],
        "flappy" => &["flappy_logic.py", "flappy.py", "test_flappy.py"],
        "encryption" => &[
            "Cargo.toml",
            "Cargo.lock",
            "src/lib.rs",
            "tests/roundtrip.rs",
        ],
        _ => &[],
    }
}

/// Commands the grader re-runs to prove the artifact is green. Each is `(label, cmd)`.
pub fn verify_commands(scenario: &str) -> &'static [(&'static str, &'static str)] {
    match scenario {
        "starter" => &[(
            "python-check",
            "python3 test_math_utils.py && python3 -c \"from math_utils import add; assert add(2,3)==5\"",
        )],
        "pong" => &[("pong-tests", "python3 test_pong.py")],
        "flappy" => &[("flappy-tests", "python3 test_flappy.py")],
        "encryption" => &[("cargo-tests-offline", "cargo test --offline")],
        _ => &[],
    }
}

/// A hidden grader: a deterministic behavioral probe of the produced workspace.
type HiddenGrader = fn(&crate::tools::WorkspaceCap) -> bool;

/// Hidden graders: extra checks that must pass for a green scenario but are not re-run as
/// shell commands (behavioral probes that are cheap and deterministic). Each entry is
/// `(label, check)`.
pub fn hidden_grader_checks(scenario: &str) -> &'static [(&'static str, HiddenGrader)] {
    match scenario {
        "starter" => &[
            ("add-returns-sum", add_grader as HiddenGrader),
            ("test-asserts-2-plus-3", test_grader as HiddenGrader),
        ],
        "pong" => &[
            ("pong-behavior-contract", pong_probe_grader as HiddenGrader),
            ("pong-runner-uses-core", pong_runner_grader as HiddenGrader),
        ],
        "flappy" => &[
            (
                "flappy-behavior-contract",
                flappy_probe_grader as HiddenGrader,
            ),
            (
                "flappy-runner-uses-core",
                flappy_runner_grader as HiddenGrader,
            ),
        ],
        "encryption" => &[
            (
                "encryption-uses-established-crate",
                encryption_crate_grader as HiddenGrader,
            ),
            (
                "encryption-exposes-api",
                encryption_api_grader as HiddenGrader,
            ),
            (
                "encryption-consumer-green",
                encryption_consumer_grader as HiddenGrader,
            ),
            (
                "encryption-removal-probe",
                encryption_removal_grader as HiddenGrader,
            ),
        ],
        _ => &[],
    }
}

mod basic;
pub use basic::{report, report_with_cap};

use basic::{
    add_grader, flappy_probe_grader, flappy_runner_grader, grader_file, pong_probe_grader,
    pong_runner_grader, test_grader,
};

mod manifest;
use manifest::{
    established_registry_packages, manifest_has_path_dep, AEAD_ALLOW, DEP_TABLES, GRAPH_MAX_NODES,
    PROBE_MAX_OUTPUT, SRC_MAX_DEPTH, SRC_MAX_FILES,
};

/// One source file of the produced crate, captured descriptor-relatively: its
/// workspace-relative path and its contents. Only `Cargo.toml` and `src/**/*.rs` are
/// collected, paths stay relative, symlinks are never followed, and the file/byte/depth
/// budgets are enforced.
struct SourceFile {
    rel: PathBuf,
    text: String,
}

/// Read `name` from a nonblocking/no-follow regular descriptor with a `cap + 1` bounded read.
fn open_read_capped(dir: &openat::Dir, name: &std::ffi::OsStr) -> Option<Vec<u8>> {
    let f = crate::tools::open_regular_os_at(dir, name).ok()?;
    let mut bytes = Vec::new();
    f.take(SCAN_MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > SCAN_MAX_BYTES {
        return None;
    }
    Some(bytes)
}

/// Walk one descriptor-relative `src` subtree with `O_NOFOLLOW` at every level,
/// collecting `*.rs` files (never descending through a symlink), bounded by
/// `SRC_MAX_DEPTH`/`SRC_MAX_FILES`, preserving relative paths.
fn walk_src_dir(
    dir: &openat::Dir,
    prefix: &Path,
    depth: usize,
    out: &mut Vec<SourceFile>,
) -> Option<()> {
    if depth > SRC_MAX_DEPTH || out.len() >= SRC_MAX_FILES {
        return None;
    }
    let mut entries = dir
        .list_dir(".")
        .ok()?
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    entries.sort_by(|a, b| a.file_name().cmp(b.file_name()));
    for ent in entries {
        match ent.simple_type() {
            Some(openat::SimpleType::Symlink) => return None,
            Some(openat::SimpleType::Dir) => {
                let sub = dir.sub_dir(ent.file_name()).ok()?;
                walk_src_dir(&sub, &prefix.join(ent.file_name()), depth + 1, out)?;
            }
            Some(openat::SimpleType::File) => {
                if !ent.file_name().to_string_lossy().ends_with(".rs") {
                    continue;
                }
                if out.len() >= SRC_MAX_FILES {
                    return None;
                }
                let bytes = open_read_capped(dir, ent.file_name())?;
                let text = String::from_utf8(bytes).ok()?;
                out.push(SourceFile {
                    rel: prefix.join(ent.file_name()),
                    text,
                });
            }
            _ => return None,
        }
    }
    Some(())
}

/// Collect the complete bounded source tree of the produced crate: `Cargo.toml` plus every
/// `src/**/*.rs`. Descriptor-relative with `O_NOFOLLOW` at every step, symlinks
/// rejected, with a bounded file count, per-file byte cap, and walk depth.
fn collect_sources(ws: &crate::tools::WorkspaceCap) -> Option<Vec<SourceFile>> {
    let mut out = Vec::new();
    let root = ws.root_dir().try_clone().ok()?;
    let manifest = open_read_capped(&root, std::ffi::OsStr::new("Cargo.toml"))?;
    out.push(SourceFile {
        rel: PathBuf::from("Cargo.toml"),
        text: String::from_utf8(manifest).ok()?,
    });
    let src = root.sub_dir("src").ok()?;
    walk_src_dir(&src, Path::new("src"), 0, &mut out)?;
    Some(out)
}

/// Whether a `use` path is rooted at one of the given crypto crate rlib names. Because the
/// prefix is built from the syn `UseTree` itself (not tokens), a locally-declared module
/// that merely names itself `aes_gcm` looks identical and fails closed elsewhere; the removal
/// probe and the behavioral consumer are what catch that rewrite.
fn use_path_is_crypto(prefix: &[String], roots: &HashSet<String>) -> bool {
    prefix.first().map(|r| roots.contains(r)).unwrap_or(false)
}

/// Expand one `use` tree into the identifiers it imports from a crypto `roots` crate: the
/// leaf names (and rename targets) of every path whose first segment is a crypto rlib name.
/// A glob import contributes nothing (fail closed for dynamic structures).
fn collect_use_tree(
    tree: &syn::UseTree,
    prefix: &mut Vec<String>,
    roots: &HashSet<String>,
    out: &mut HashSet<String>,
) {
    match tree {
        syn::UseTree::Path(p) => {
            prefix.push(p.ident.to_string());
            collect_use_tree(&p.tree, prefix, roots, out);
            prefix.pop();
        }
        syn::UseTree::Name(n) => {
            if use_path_is_crypto(prefix, roots) {
                out.insert(n.ident.to_string());
            }
        }
        syn::UseTree::Rename(r) => {
            if use_path_is_crypto(prefix, roots) {
                out.insert(r.rename.to_string());
            }
        }
        syn::UseTree::Group(g) => {
            for item in &g.items {
                collect_use_tree(item, prefix, roots, out);
            }
        }
        syn::UseTree::Glob(_) => {}
    }
}

/// Whether a local binding provably resolves to a recognized crypto-derived value. `Crypto`
/// records the constructing type's name; `Unknown` covers everything we cannot prove (fail
/// closed on unresolved shapes).
mod evidence;
mod flow;
mod reach;
use evidence::{build_crate_evidence, CrateEvidence};
use flow::{exported_op_flows_to_return, OpDir};

/// The encryption manifest must declare an established AEAD/stream crate as a real registry
/// `[dependencies]`-table entry whose exported `encrypt`/`decrypt` **both** provably
/// return the result of a recognized authenticated operation on a crypto-derived receiver,
/// each direction analyzed independently through the bounded same-crate call graph. All of this is
/// proven on a real TOML parse and the syn AST: a constructor or an arbitrary method that is
/// constructed, called, or discarded without its result reaching the returned value, plus an unused
/// dependency, a bare import, a `type_name` marker, a dead helper, a comment, a string,
/// a macro, an unreachable branch, or fake local `encrypt`/`decrypt` methods, never count.
fn encryption_crate_grader(ws: &crate::tools::WorkspaceCap) -> bool {
    if manifest_has_path_dep(ws) {
        return false;
    }
    let packages = established_registry_packages(ws);
    if packages.is_empty() {
        return false;
    }
    let roots: HashSet<String> = AEAD_ALLOW
        .iter()
        .filter(|(name, _)| packages.iter().any(|p| p == name))
        .map(|(_, rlib)| (*rlib).to_string())
        .collect();
    if roots.is_empty() {
        return false;
    }
    let Some(ev) = build_crate_evidence(ws, &roots) else {
        return false;
    };
    // Both directions must be proven independently: the exported `encrypt` must return a value
    // derived from an authenticated encrypt operation and the exported `decrypt` from an
    // authenticated decrypt operation. A codec whose encrypt uses real crypto but whose decrypt is
    // hand-rolled (or that discards a real operation's result) can never pass.
    if !ev.is_exported("encrypt") || !exported_op_flows_to_return("encrypt", OpDir::Encrypt, &ev) {
        return false;
    }
    if !ev.is_exported("decrypt") || !exported_op_flows_to_return("decrypt", OpDir::Decrypt, &ev) {
        return false;
    }
    true
}

/// The produced crate's manifest must declare a simple, path-referencable package name.
fn encryption_package_name(ws: &crate::tools::WorkspaceCap) -> Option<String> {
    let mani = grader_file(ws, "Cargo.toml")?;
    let parsed: toml::Value = toml::from_str(&mani).ok()?;
    let name = parsed.get("package")?.get("name")?.as_str()?;
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !ok {
        return None;
    }
    Some(name.to_string())
}

/// The encryption library must directly expose `pub fn encrypt` and `pub fn decrypt` from
/// `src/lib.rs`. A module item, re-export, macro, conditional item, comment, or string cannot
/// satisfy this check.
fn encryption_api_grader(ws: &crate::tools::WorkspaceCap) -> bool {
    let roots = HashSet::new();
    let Some(ev) = build_crate_evidence(ws, &roots) else {
        return false;
    };
    ["encrypt", "decrypt"].iter().all(|n| ev.is_exported(n))
}

/// The encryption hidden grader: build a temporary external Rust consumer that depends on the
/// produced crate by path and run `cargo test --offline` with grader-authored assertions
/// for ciphertext != plaintext, roundtrip, wrong-password failure, tamper failure,
/// non-reused ciphertext across repeats, minimum AEAD overhead, and empty plaintext. The
/// produced crate's own tests are never used as hidden evidence. A crate that is really
/// crypto must still fail the crypto-removal probe, so a self-consistent fake that needs
/// no acknowledged crate is rejected here too.
fn encryption_consumer_grader(ws: &crate::tools::WorkspaceCap) -> bool {
    if !encryption_crate_grader(ws) {
        return false;
    }
    let Some(name) = encryption_package_name(ws) else {
        return false;
    };
    let Ok(base) = crate::harness::fresh_private_dir("llxprt-rs-consumer") else {
        return false;
    };
    let produced = base.join("produced");
    let consumer = base.join("consumer");
    let consumer_passed =
        materialize_sources(ws, &produced) && build_and_test_consumer(&consumer, &produced, &name);
    let probe = probe_crypto_removal(&base.join("pruned"), ws, &name);
    let _ = std::fs::remove_dir_all(&base);
    consumer_probe_proves_crypto(consumer_passed, probe)
}

fn materialize_sources(ws: &crate::tools::WorkspaceCap, destination: &Path) -> bool {
    let Some(sources) = collect_sources(ws) else {
        return false;
    };
    if !sources
        .iter()
        .any(|source| source.rel == Path::new("Cargo.toml"))
    {
        return false;
    }
    for source in sources {
        let path = destination.join(source.rel);
        let Some(parent) = path.parent() else {
            return false;
        };
        if std::fs::create_dir_all(parent).is_err() || std::fs::write(path, source.text).is_err() {
            return false;
        }
    }
    true
}

/// The grader-authored behavioral contract, shared by the external consumer and the removal
/// probe: nonidentity, roundtrip, wrong-password, tamper, repeated-ciphertext-differs,
/// multiple repeats, minimum AEAD overhead, and empty plaintext.
fn contract_tests(name: &str) -> String {
    format!(
        r#"use {name}::{{decrypt, encrypt}};

#[test]
fn ciphertext_differs_from_plaintext() {{    let ct = encrypt("pw", b"attack at dawn").unwrap();
    assert_ne!(ct, b"attack at dawn");
}}

#[test]
fn roundtrip_works() {{
    let ct = encrypt("pw", b"hello world roundtrip").unwrap();
    assert_eq!(decrypt("pw", &ct).unwrap(), b"hello world roundtrip");
}}

#[test]
fn wrong_password_fails() {{
    let ct = encrypt("right pass", b"sensitive data").unwrap();
    assert!(decrypt("wrong pass", &ct).is_err());
}}

#[test]
fn tamper_fails() {{
    let ct = encrypt("pw", b"tamper me").unwrap();
    let mut t = ct.clone();
    *t.last_mut().unwrap() ^= 0xff;
    assert!(decrypt("pw", &t).is_err());
}}

#[test]
fn repeated_encrypt_must_differ() {{
    let pt = b"fixed plaintext, fixed password";
    let first = encrypt("a fixed password", pt).unwrap();
    let second = encrypt("a fixed password", pt).unwrap();
    assert_ne!(first, second);
}}

#[test]
fn repeated_encrypt_all_differ() {{
    let pts: [&[u8]; 2] = [b"alpha", b"beta"];
    for pt in pts {{
        let mut seen: Vec<Vec<u8>> = Vec::new();
        for _ in 0..{DISTINCT_ENCRYPT_PASSES} {{
            let ct = encrypt("same pw", pt).unwrap();
            assert!(!seen.contains(&ct));
            seen.push(ct);
        }}
    }}
}}

#[test]
fn ciphertext_carries_aead_overhead() {{
    let ct = encrypt("pw", b"plain").unwrap();
    assert!(ct.len() >= b"plain".len() + {MIN_OVERHEAD});
}}

#[test]
fn empty_plaintext_roundtrips() {{
    let ct = encrypt("pw", b"").unwrap();
    assert_ne!(ct, b"");
    assert_eq!(decrypt("pw", &ct).unwrap(), b"");
}}
"#
    )
}
/// Whether an inline-table dependency value is (or inherits) an established crypto package.
fn dep_entry_is_crypto(
    key: &str,
    v: &toml::Value,
    wdeps: Option<&toml::map::Map<String, toml::Value>>,
) -> bool {
    let crypto: Vec<&str> = AEAD_ALLOW.iter().map(|(n, _)| *n).collect();
    match v {
        toml::Value::String(_) => crypto.contains(&key),
        toml::Value::Table(t) => {
            let k: &str = t.get("package").and_then(|p| p.as_str()).unwrap_or(key);
            if crypto.contains(&k) {
                return true;
            }
            if t.get("workspace").and_then(|w| w.as_bool()) == Some(true) {
                if let Some(wt) = wdeps.and_then(|m| m.get(key)).and_then(|w| w.as_table()) {
                    let wk: &str = wt.get("package").and_then(|p| p.as_str()).unwrap_or(key);
                    if crypto.contains(&wk) {
                        return true;
                    }
                }
            }
            false
        }
        _ => false,
    }
}

/// Rewrite the produced manifest with every established crypto dependency removed from all
/// dependency tables: normal/dev/build, every `[target.*]` variant, `workspace = true`
/// inheritance, and the inherited `[workspace.dependencies]` table. Alias keys pointing
/// at an established package are dropped too, so the probe cannot keep a renamed dep alive.
fn prune_crypto_manifest(text: &str) -> Option<String> {
    let mut root: toml::Value = toml::from_str(text).ok()?;
    let root_t = root.as_table().cloned()?;
    let wdeps = root_t
        .get("workspace")
        .and_then(|w| w.as_table())
        .and_then(|w| w.get("dependencies"))
        .and_then(|d| d.as_table())
        .cloned();
    fn drop_crypto(
        t: &mut toml::map::Map<String, toml::Value>,
        wdeps: &Option<toml::map::Map<String, toml::Value>>,
    ) {
        let mut keep: Vec<(String, toml::Value)> = Vec::new();
        let keys: Vec<String> = t.keys().cloned().collect();
        for k in keys {
            if let Some(v) = t.get(&k).cloned() {
                if !dep_entry_is_crypto(&k, &v, wdeps.as_ref()) {
                    keep.push((k, v));
                }
            }
        }
        t.clear();
        for (k, v) in keep {
            t.insert(k, v);
        }
    }
    if let Some(t) = root_t.get("dependencies").and_then(|d| d.as_table()) {
        let mut t = t.clone();
        drop_crypto(&mut t, &wdeps);
        root.as_table_mut()?
            .insert("dependencies".to_string(), toml::Value::Table(t));
    }
    if let Some(t) = root_t.get("dev-dependencies").and_then(|d| d.as_table()) {
        let mut t = t.clone();
        drop_crypto(&mut t, &wdeps);
        root.as_table_mut()?
            .insert("dev-dependencies".to_string(), toml::Value::Table(t));
    }
    if let Some(t) = root_t.get("build-dependencies").and_then(|d| d.as_table()) {
        let mut t = t.clone();
        drop_crypto(&mut t, &wdeps);
        root.as_table_mut()?
            .insert("build-dependencies".to_string(), toml::Value::Table(t));
    }
    if let Some(targets) = root_t.get("target").and_then(|x| x.as_table()) {
        let mut targets = targets.clone();
        let target_keys: Vec<String> = targets.keys().cloned().collect();
        for tk in target_keys {
            let Some(mut tt) = targets.get(&tk).and_then(|v| v.as_table()).cloned() else {
                continue;
            };
            for tbl in DEP_TABLES {
                let Some(t) = tt.get(tbl).and_then(|d| d.as_table()) else {
                    continue;
                };
                let mut t = t.clone();
                drop_crypto(&mut t, &wdeps);
                tt.insert(tbl.to_string(), toml::Value::Table(t));
            }
            targets.insert(tk, toml::Value::Table(tt));
        }
        root.as_table_mut()?
            .insert("target".to_string(), toml::Value::Table(targets));
    }
    if let Some(wt) = root_t.get("workspace").and_then(|w| w.as_table()) {
        let mut wt = wt.clone();
        if let Some(dt) = wt.get("dependencies").and_then(|d| d.as_table()).cloned() {
            let mut dt = dt;
            drop_crypto(&mut dt, &None);
            wt.insert("dependencies".to_string(), toml::Value::Table(dt));
        }
        root.as_table_mut()?
            .insert("workspace".to_string(), toml::Value::Table(wt));
    }
    toml::to_string(&root).ok()
}

/// The outcome of the dependency-removal probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemovalProbe {
    /// The crypto-free rebuild still compiles and passes the grader-authored behavioral
    /// tests, so the produced `encrypt`/`decrypt` never actually needed the crate.
    Green,
    /// The rebuild failed with a missing-crate compile error, consistent with a crate that
    /// genuinely depends on the acknowledged crypto crate. Supplementary evidence only:
    /// it is never *required* and never proof by itself.
    ExpectedCryptoCompileFailure,
    /// Anything else (an unrelated compile error, a malformed manifest, a setup/spawn
    /// problem, a timeout). Carries no evidence either way and must never be treated as
    /// proof that the crate is (or is not) crypto.
    Inconclusive,
}

/// Classify a probe run into one of the three tri-state outcomes. Only a clean exit is
/// `Green`; only a missing-crate compile error is `ExpectedCryptoCompileFailure`; a
/// timeout, a spawn/setup failure, or any unrelated compiler error is `Inconclusive`.
fn classify_probe(o: &crate::process::CmdOutcome) -> RemovalProbe {
    if o.timed_out {
        return RemovalProbe::Inconclusive;
    }
    if o.status == Some(0) {
        return RemovalProbe::Green;
    }
    let out = String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr);
    let mention_missing_crate = [
        "E0432",
        "E0433",
        "failed to resolve: use of undeclared crate",
        "unresolved import",
    ]
    .iter()
    .any(|m| out.contains(m));
    let mentions_crypto = AEAD_ALLOW.iter().any(|(_, rlib)| out.contains(*rlib));
    if mention_missing_crate && mentions_crypto {
        RemovalProbe::ExpectedCryptoCompileFailure
    } else {
        RemovalProbe::Inconclusive
    }
}

/// Removal probe: copy the complete bounded `src` tree and the pruned manifest (all crypto
/// aliases stripped everywhere) off under `base`, then run grader-authored behavioral tests
/// offline against the copy. `Green` means the crypto-free rebuild still compiles and all
/// contract tests pass, i.e. the produced `encrypt`/`decrypt` never actually needed the
/// acknowledged crate. A missing-crate compile error is classified as expected, and every other
/// failure mode is `Inconclusive` (see [`RemovalProbe`]).
fn probe_crypto_removal(
    base: &std::path::Path,
    ws: &crate::tools::WorkspaceCap,
    name: &str,
) -> RemovalProbe {
    let Some(sources) = collect_sources(ws) else {
        return RemovalProbe::Inconclusive;
    };
    for s in &sources {
        if s.rel.extension().map(|e| e == "rs").unwrap_or(false) {
            let dest = base.join(&s.rel);
            let Some(parent) = dest.parent() else {
                continue;
            };
            if std::fs::create_dir_all(parent).is_err() {
                return RemovalProbe::Inconclusive;
            }
            if std::fs::write(&dest, &s.text).is_err() {
                return RemovalProbe::Inconclusive;
            }
        }
    }
    let Some(mani) = sources.iter().find(|s| s.rel == Path::new("Cargo.toml")) else {
        return RemovalProbe::Inconclusive;
    };
    let Some(pruned) = prune_crypto_manifest(&mani.text) else {
        return RemovalProbe::Inconclusive;
    };
    if std::fs::create_dir_all(base.join("tests")).is_err() {
        return RemovalProbe::Inconclusive;
    }
    if std::fs::write(base.join("Cargo.toml"), pruned).is_err() {
        return RemovalProbe::Inconclusive;
    }
    if std::fs::write(base.join("tests/contract.rs"), contract_tests(name)).is_err() {
        return RemovalProbe::Inconclusive;
    }
    run_removal_command("cargo", base)
}

fn run_removal_command(program: &str, base: &Path) -> RemovalProbe {
    let outcome = process::run_cmd(CmdSpec {
        program: program.to_string(),
        args: vec!["test".to_string(), "--offline".to_string()],
        cwd: Some(base.to_path_buf()),
        cwd_fd: None,
        env_add: Vec::new(),
        timeout: CONSUMER_TIMEOUT,
        max_output: PROBE_MAX_OUTPUT,
    });
    match outcome {
        Ok(outcome) => classify_probe(&outcome),
        Err(_) => RemovalProbe::Inconclusive,
    }
}

fn removal_probe_proves_crypto(probe: RemovalProbe) -> bool {
    probe == RemovalProbe::ExpectedCryptoCompileFailure
}

fn consumer_probe_proves_crypto(consumer_passed: bool, probe: RemovalProbe) -> bool {
    consumer_passed && removal_probe_proves_crypto(probe)
}

/// Hidden check: removing every acknowledged crypto dependency must produce the expected
/// missing-crypto compile failure. A crypto-free green build and every inconclusive setup,
/// process, timeout, or unrelated compiler failure fail closed.
fn encryption_removal_grader(ws: &crate::tools::WorkspaceCap) -> bool {
    if !encryption_crate_grader(ws) {
        return false;
    }
    let Some(name) = encryption_package_name(ws) else {
        return false;
    };
    let Ok(base) = crate::harness::fresh_private_dir("llxprt-rs-prune") else {
        return false;
    };
    let probe = probe_crypto_removal(&base, ws, &name);
    let _ = std::fs::remove_dir_all(&base);
    removal_probe_proves_crypto(probe)
}

/// Write the consumer crate and run its `cargo test --offline`.
fn build_and_test_consumer(base: &std::path::Path, ws: &std::path::Path, name: &str) -> bool {
    let manifest = format!(
        "[package]\nname = \"filecrypt-consumer\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n{name} = {{ path = {ws:?} }}\n"
    );
    let tests = contract_tests(name);
    if std::fs::create_dir_all(base.join("tests")).is_err() {
        return false;
    }
    if std::fs::write(base.join("Cargo.toml"), manifest).is_err() {
        return false;
    }
    if std::fs::write(base.join("tests/contract.rs"), tests).is_err() {
        return false;
    }
    let o = match process::run_cmd(CmdSpec {
        program: "cargo".to_string(),
        args: vec!["test".to_string(), "--offline".to_string()],
        cwd: Some(base.to_path_buf()),
        cwd_fd: None,
        env_add: Vec::new(),
        timeout: CONSUMER_TIMEOUT,
        max_output: CONSUMER_MAX_OUTPUT,
    }) {
        Ok(o) => o,
        Err(_) => return false,
    };
    o.status == Some(0) && !o.timed_out
}

/// The result of running one verification command.
#[derive(Debug, Clone)]
pub struct Verification {
    pub label: &'static str,
    pub command: &'static str,
    pub passed: bool,
    pub tail: String,
}

/// Graded evidence for one scenario. The verification commands are each run **exactly once**
/// here, and the same evidence drives both the pass/fail decision and the report JSON.
#[derive(Debug, Clone)]
pub struct ScenarioEvidence {
    pub passed: bool,
    pub protocol_pass: bool,
    pub tool_use_pass: bool,
    pub build_test_pass: bool,
    pub structural_pass: bool,
    pub hidden_graders_pass: bool,
    pub protocol_score: f64,
    pub tool_use_score: f64,
    pub build_test_score: f64,
    pub structural_score: f64,
    pub tool_count: usize,
    pub turns_run: usize,
    pub verifications: Vec<Verification>,
    pub hidden_graders: Vec<(String, bool)>,
    pub inventory: Inventory,
}

/// Run a single verification command, returning `(passed, output_tail)`. The real
/// test/build is executed through the shared bounded runner so a hanging test cannot block
/// the harness.
pub fn try_verify(workspace: &crate::tools::WorkspaceCap, command: &str) -> (bool, String) {
    run_verification(workspace, command, Vec::new())
}

fn run_verification(
    workspace: &crate::tools::WorkspaceCap,
    command: &str,
    env_add: Vec<(String, String)>,
) -> (bool, String) {
    let o = match process::run_cmd(CmdSpec {
        program: "bash".to_string(),
        args: vec!["-c".to_string(), command.to_string()],
        cwd: None,
        cwd_fd: Some(workspace_fd(workspace)),
        env_add,
        timeout: std::time::Duration::from_secs(300),
        max_output: 64 * 1024,
    }) {
        Ok(o) => o,
        Err(e) => return (false, format!("could not run: {e}")),
    };
    let out = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    (
        o.status == Some(0) && !o.timed_out,
        out.chars().take(800).collect(),
    )
}

/// Build the graded evidence for a scenario from the `results` already collected. Every
/// verification command runs exactly once here, and every structural/hidden check walks the
/// workspace no-follow. The caller reuses this value for both the pass/fail decision and
/// the report.
fn evidence_with_cap(
    scenario: &str,
    ws: &crate::tools::WorkspaceCap,
    results: &[crate::harness::BbResult],
) -> ScenarioEvidence {
    // Run real verifications first so (for encryption) `cargo test --offline` materializes
    // Cargo.lock before the structural walk.
    let mut verifications = Vec::new();
    for (label, cmd) in verify_commands(scenario) {
        let (ok, tail) = try_verify(ws, cmd);
        verifications.push(Verification {
            label,
            command: cmd,
            passed: ok,
            tail,
        });
    }
    let structural_score = crate::harness::score_present_cap(ws, required_files(scenario));
    let build_test_passed = verifications.iter().filter(|v| v.passed).count();
    let build_total = verifications.len();

    // The aggregate tool-call count is saturated (never a wrapping/overflow panic), so a
    // pathologically large reported count cannot crash the grader on any host.
    let tool_count: usize = results
        .iter()
        .fold(0usize, |acc, r| acc.saturating_add(r.tool_calls));
    let all_ok = !results.is_empty() && results.iter().all(|r| r.ok);
    let protocol_score = if all_ok { 1.0 } else { 0.0 };
    let tool_use_score = if tool_count >= 2 {
        1.0
    } else {
        tool_count as f64 / 2.0
    };

    let hidden_checks = hidden_grader_checks(scenario);
    let hidden_graders: Vec<(String, bool)> = hidden_checks
        .iter()
        .map(|(label, c)| (label.to_string(), c(ws)))
        .collect();
    let hidden_passed = hidden_graders.iter().filter(|(_, ok)| *ok).count();
    let hidden_graders_pass = !hidden_graders.is_empty() && hidden_passed == hidden_graders.len();

    let build_test_score = if build_total == 0 {
        0.0
    } else {
        build_test_passed as f64 / build_total as f64
    };

    let protocol_pass = all_ok;
    let tool_use_pass = tool_count >= 2;
    let build_test_pass = build_test_score >= 1.0;
    let structural_pass = structural_score >= 1.0;
    let passed =
        protocol_pass && tool_use_pass && build_test_pass && structural_pass && hidden_graders_pass;

    ScenarioEvidence {
        passed,
        protocol_pass,
        tool_use_pass,
        build_test_pass,
        structural_pass,
        hidden_graders_pass,
        protocol_score,
        tool_use_score,
        build_test_score,
        structural_score,
        tool_count,
        turns_run: results.len(),
        verifications,
        hidden_graders,
        inventory: crate::harness::inventory_cap(ws),
    }
}
/// Path-based convenience wrapper for isolated callers. The parity runner uses
/// [`report_with_cap`] so it never re-resolves the workspace after agent execution.
pub fn evidence(
    scenario: &str,
    workspace: &Path,
    results: &[crate::harness::BbResult],
) -> ScenarioEvidence {
    match crate::tools::WorkspaceCap::open(workspace) {
        Ok(capability) => evidence_with_cap(scenario, &capability, results),
        Err(_) => ScenarioEvidence {
            passed: false,
            protocol_pass: false,
            tool_use_pass: false,
            build_test_pass: false,
            structural_pass: false,
            hidden_graders_pass: false,
            protocol_score: 0.0,
            tool_use_score: 0.0,
            build_test_score: 0.0,
            structural_score: 0.0,
            tool_count: 0,
            turns_run: results.len(),
            verifications: Vec::new(),
            hidden_graders: Vec::new(),
            inventory: Inventory {
                files: Vec::new(),
                truncated: false,
            },
        },
    }
}

#[cfg(test)]
mod tests;
