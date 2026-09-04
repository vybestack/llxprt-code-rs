//! Release-gate orchestration.
//!
//! The security-sensitive source archive builder and verifier remain in their reviewed
//! Bash/Python implementations.  This module owns the ordered gate plan, temporary target
//! directories, audit lockfile discovery, and GNU-tar reproducibility proof.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const TOOLCHAIN: &str = "+1.88.0";

pub fn run_release_gates(root: &Path) -> Result<(), String> {
    require_python(root)?;
    let archive = command_output(
        root,
        "python3",
        &["scripts/release-version.py", "--value", "archive"],
    )?;

    heading("checksum-locked registry source closure");
    run(root, "python3", &["scripts/verify-registry-vendor.py"])?;
    run_format(root)?;
    heading("production module coupling debt");
    crate::coupling::run(root)?;
    run_xtask_checks(root)?;
    run_release_fixtures(root)?;
    run_vendor_policy_fixtures(root)?;
    heading("published envelope schema drift");
    cargo(root, &["xtask", "envelope-schema", "--check"])?;
    run_workspace_checks(root)?;
    run_direct_vendor_checks(root)?;
    run_msrv_and_docs(root)?;

    heading("vendor + license inventory");
    run(root, "bash", &["scripts/verify-vendor-licenses.sh"])?;
    run_local_audit(root)?;

    heading("release build (source tree, vendor path deps)");
    cargo(
        root,
        &[
            "build",
            "--offline",
            "--release",
            "--locked",
            "--workspace",
            "--all-features",
        ],
    )?;
    build_and_compare_source_bundle(root, &archive)?;
    println!("all release gates passed");
    Ok(())
}

pub fn run_release_fixtures(root: &Path) -> Result<(), String> {
    heading("source and release publication adversarial cases");
    run(root, "python3", &["scripts/verify-source-object-policy.py"])?;
    run(root, "python3", &["scripts/test-source-oci-publication.py"])?;
    run(root, "bash", &["scripts/test-source-bundle-verifier.sh"])?;
    run(root, "bash", &["scripts/test-release-workflow.sh"])
}

pub fn run_source_bundle(root: &Path, args: &[String]) -> Result<(), String> {
    let (script, script_args): (&str, Vec<&str>) = match args {
        [operation] if operation == "build" => ("scripts/build-source-bundle.sh", Vec::new()),
        [operation, output] if operation == "build" => {
            ("scripts/build-source-bundle.sh", vec![output.as_str()])
        }
        [operation] if operation == "list" => ("scripts/build-source-bundle.sh", vec!["--list"]),
        [operation] if operation == "verify" => ("scripts/verify-source-bundle.sh", Vec::new()),
        [operation, bundle] if operation == "verify" => {
            ("scripts/verify-source-bundle.sh", vec![bundle.as_str()])
        }
        [operation] if operation == "test" => {
            return run(root, "bash", &["scripts/test-source-bundle-verifier.sh"]);
        }
        _ => return Err(source_bundle_usage()),
    };
    run_dynamic(root, "bash", std::iter::once(script).chain(script_args))
}

pub fn source_bundle_usage() -> String {
    "usage: cargo xtask source-bundle <build [OUT]|list|verify [BUNDLE]|test>".to_string()
}

fn run_format(root: &Path) -> Result<(), String> {
    heading("fmt");
    cargo(root, &["fmt", "--all", "--", "--check"])?;
    cargo(
        root,
        &[
            "fmt",
            "--all",
            "--manifest-path",
            "xtask/Cargo.toml",
            "--",
            "--check",
        ],
    )
}

fn run_xtask_checks(root: &Path) -> Result<(), String> {
    heading("xtask tests and lint");
    cargo(
        root,
        &[
            "test",
            "--offline",
            "--locked",
            "--manifest-path",
            "xtask/Cargo.toml",
        ],
    )?;
    cargo(
        root,
        &[
            "clippy",
            "--offline",
            "--locked",
            "--manifest-path",
            "xtask/Cargo.toml",
            "--all-targets",
            "--all-features",
            "--",
            "-D",
            "warnings",
        ],
    )?;
    heading("production Rust LOC and complexity");
    cargo(root, &["xtask", "quality"])
}

fn run_vendor_policy_fixtures(root: &Path) -> Result<(), String> {
    heading("vendor provenance regression cases");
    for script in [
        "scripts/test-vendor-provenance.sh",
        "scripts/test-dependency-inventory.sh",
        "scripts/test-vendor-license.sh",
        "scripts/test-upstream-evidence.sh",
    ] {
        run(root, "bash", &[script])?;
    }
    heading("resolved provider feature graph");
    run(root, "bash", &["scripts/test-provider-features.sh"])?;
    run(root, "python3", &["scripts/verify-provider-features.py"])
}

fn run_workspace_checks(root: &Path) -> Result<(), String> {
    heading("clippy (all targets, warnings denied)");
    cargo(
        root,
        &[
            "clippy",
            "--offline",
            "--locked",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--",
            "-D",
            "warnings",
        ],
    )?;
    heading("MSRV clippy (Rust 1.88, all targets, warnings denied)");
    cargo(
        root,
        &[
            "clippy",
            "--offline",
            "--locked",
            "--manifest-path",
            "xtask/Cargo.toml",
            "--all-targets",
            "--all-features",
            "--",
            "-D",
            "warnings",
        ],
    )?;
    cargo(
        root,
        &[
            "clippy",
            "--offline",
            "--locked",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--",
            "-D",
            "warnings",
        ],
    )?;
    heading("tests");
    cargo(
        root,
        &[
            "test",
            "--offline",
            "--locked",
            "--workspace",
            "--all-targets",
            "--all-features",
        ],
    )
}

fn run_direct_vendor_checks(root: &Path) -> Result<(), String> {
    heading("direct vendored SerdesAI feature surfaces and OpenAI tests");
    let target = TempDir::new("llxprt-vendor-target")?;
    run_with_env(
        root,
        "bash",
        &["scripts/test-vendor-feature-surfaces.sh"],
        &[("CARGO_TARGET_DIR", target.path().as_os_str())],
    )?;
    vendor_cargo(
        root,
        &target,
        "clippy",
        "vendor/serdes-ai-responses/Cargo.toml",
        &[],
    )?;
    vendor_cargo(
        root,
        &target,
        "test",
        "vendor/serdes-ai-responses/Cargo.toml",
        &[],
    )?;
    let provider_features = ["--no-default-features", "--features", "openai"];
    vendor_cargo(
        root,
        &target,
        "clippy",
        "vendor/serdes-ai-providers/Cargo.toml",
        &provider_features,
    )?;
    vendor_cargo(
        root,
        &target,
        "test",
        "vendor/serdes-ai-providers/Cargo.toml",
        &provider_features,
    )?;
    let model_features = ["--features", "openai"];
    vendor_cargo(
        root,
        &target,
        "clippy",
        "vendor/serdes-ai-models/Cargo.toml",
        &model_features,
    )?;
    vendor_cargo(
        root,
        &target,
        "test",
        "vendor/serdes-ai-models/Cargo.toml",
        &model_features,
    )
}

fn vendor_cargo(
    root: &Path,
    target: &TempDir,
    operation: &str,
    manifest: &str,
    features: &[&str],
) -> Result<(), String> {
    let mut args = vec![
        TOOLCHAIN,
        operation,
        "--offline",
        "--locked",
        "--manifest-path",
        manifest,
    ];
    args.extend_from_slice(features);
    if operation == "clippy" {
        args.extend_from_slice(&["--all-targets", "--", "-D", "warnings"]);
    }
    run_with_env(
        root,
        "cargo",
        &args,
        &[("CARGO_TARGET_DIR", target.path().as_os_str())],
    )
}

fn run_msrv_and_docs(root: &Path) -> Result<(), String> {
    heading("MSRV (rust-version = 1.88)");
    cargo(
        root,
        &[
            "check",
            "--offline",
            "--locked",
            "--manifest-path",
            "xtask/Cargo.toml",
            "--all-targets",
            "--all-features",
        ],
    )?;
    cargo(
        root,
        &[
            "check",
            "--offline",
            "--locked",
            "--workspace",
            "--all-targets",
            "--all-features",
        ],
    )?;
    heading("rustdoc (warnings denied)");
    cargo_with_env(
        root,
        &[
            "doc",
            "--offline",
            "--locked",
            "--manifest-path",
            "xtask/Cargo.toml",
            "--all-features",
            "--no-deps",
        ],
        &[("RUSTDOCFLAGS", OsStr::new("-D warnings"))],
    )?;
    cargo_with_env(
        root,
        &[
            "doc",
            "--offline",
            "--locked",
            "--workspace",
            "--all-features",
            "--no-deps",
        ],
        &[("RUSTDOCFLAGS", OsStr::new("-D warnings"))],
    )
}

fn run_local_audit(root: &Path) -> Result<(), String> {
    if env::var_os("LLXPRT_SKIP_LOCAL_AUDIT").as_deref() == Some(OsStr::new("1")) {
        println!("== local cargo audit skipped; CI runs the separate pinned audit job ==");
        return Ok(());
    }
    if !command_succeeds(root, "cargo", &["audit", "--version"]) {
        println!("!! cargo-audit not installed; local offline audit was not run");
        return Ok(());
    }
    heading("cargo audit against the local advisory cache (no fetch)");
    run(root, "cargo", &["audit", "--no-fetch"])?;
    run(
        root,
        "cargo",
        &["audit", "--no-fetch", "--file", "xtask/Cargo.lock"],
    )?;
    for lockfile in vendor_lockfiles(root)? {
        let args = vec![
            OsString::from("audit"),
            OsString::from("--no-fetch"),
            OsString::from("--file"),
            lockfile.into_os_string(),
        ];
        run_os(root, "cargo", &args, &[])?;
    }
    Ok(())
}

fn vendor_lockfiles(root: &Path) -> Result<Vec<PathBuf>, String> {
    let vendor = root.join("vendor");
    let entries =
        fs::read_dir(&vendor).map_err(|error| format!("read {}: {error}", vendor.display()))?;
    let mut locks = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("read {} entry: {error}", vendor.display()))?;
        // Match vendor/*/Cargo.lock: a Bash glob does not select hidden directories.
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        let lock = path.join("Cargo.lock");
        if lock.is_file() {
            locks.push(lock.strip_prefix(root).unwrap_or(&lock).to_path_buf());
        }
    }
    locks.sort();
    Ok(locks)
}

fn build_and_compare_source_bundle(root: &Path, archive: &str) -> Result<(), String> {
    heading(
        "source bundle build + verify (extract -> test --offline -> build --release --offline)",
    );
    let output = Path::new("dist").join(archive);
    run_build_with_umask(root, "022", &output)?;
    if !gnu_tar(root) {
        return Ok(());
    }
    heading("GNU tar source-bundle byte reproducibility");
    let comparison = TempDir::new("llxprt-bundle-comparison")?;
    let comparison_bundle = comparison.path().join(archive);
    run_build_with_umask(root, "077", &comparison_bundle)?;
    run_os(
        root,
        "cmp",
        &[output.into_os_string(), comparison_bundle.into_os_string()],
        &[],
    )
}

fn run_build_with_umask(root: &Path, mask: &str, output: &Path) -> Result<(), String> {
    let args = [
        OsString::from("-c"),
        OsString::from("umask \"$1\"; shift; exec \"$@\""),
        OsString::from("xtask-umask"),
        OsString::from(mask),
        OsString::from("bash"),
        OsString::from("scripts/build-source-bundle.sh"),
        output.as_os_str().to_os_string(),
    ];
    run_os(root, "bash", &args, &[])
}

fn require_python(root: &Path) -> Result<(), String> {
    if command_succeeds(root, "python3", &["--version"]) {
        Ok(())
    } else {
        Err("python3 (for scripts/source-bundle-validate.py) is required for release gates".into())
    }
}

fn gnu_tar(root: &Path) -> bool {
    command_output(root, "tar", &["--version"])
        .map(|output| output.contains("GNU"))
        .unwrap_or(false)
}

fn cargo(root: &Path, args: &[&str]) -> Result<(), String> {
    let mut full_args = vec![TOOLCHAIN];
    full_args.extend_from_slice(args);
    run(root, "cargo", &full_args)
}

fn cargo_with_env(
    root: &Path,
    args: &[&str],
    environment: &[(&str, &OsStr)],
) -> Result<(), String> {
    let mut full_args = vec![TOOLCHAIN];
    full_args.extend_from_slice(args);
    run_with_env(root, "cargo", &full_args, environment)
}

fn run(root: &Path, program: &str, args: &[&str]) -> Result<(), String> {
    run_dynamic(root, program, args.iter().copied())
}

fn run_dynamic<'a>(
    root: &Path,
    program: &str,
    args: impl IntoIterator<Item = &'a str>,
) -> Result<(), String> {
    let args: Vec<OsString> = args.into_iter().map(OsString::from).collect();
    run_os(root, program, &args, &[])
}

fn run_with_env(
    root: &Path,
    program: &str,
    args: &[&str],
    environment: &[(&str, &OsStr)],
) -> Result<(), String> {
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    run_os(root, program, &args, environment)
}

fn run_os(
    root: &Path,
    program: &str,
    args: &[OsString],
    environment: &[(&str, &OsStr)],
) -> Result<(), String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(root)
        .env("PYTHONDONTWRITEBYTECODE", "1");
    for (name, value) in environment {
        command.env(name, value);
    }
    let status = command
        .status()
        .map_err(|error| format!("run {program}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format_command_failure(program, args, status.to_string()))
    }
}

fn command_output(root: &Path, program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(root)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .map_err(|error| format!("run {program}: {error}"))?;
    if !output.status.success() {
        let os_args: Vec<OsString> = args.iter().map(OsString::from).collect();
        return Err(format_command_failure(
            program,
            &os_args,
            output.status.to_string(),
        ));
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_string())
        .map_err(|error| format!("{program} output is not UTF-8: {error}"))
}

fn command_succeeds(root: &Path, program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .current_dir(root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn format_command_failure(program: &str, args: &[OsString], status: String) -> String {
    let rendered = args
        .iter()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    format!("{program} {rendered} failed with {status}")
}

fn heading(text: &str) {
    println!("== {text} ==");
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Result<Self, String> {
        let base = env::var_os("TMPDIR").map_or_else(env::temp_dir, PathBuf::from);
        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("system clock before Unix epoch: {error}"))?
            .as_nanos();
        for _ in 0..100 {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!(
                "{prefix}.{}.{}.{}",
                std::process::id(),
                epoch,
                sequence
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(format!(
                        "create temporary directory {}: {error}",
                        path.display()
                    ))
                }
            }
        }
        Err(format!(
            "could not allocate a temporary {prefix} directory in {}",
            base.display()
        ))
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.path) {
            eprintln!(
                "warning: remove temporary directory {}: {error}",
                self.path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::{source_bundle_usage, vendor_lockfiles, TempDir};

    #[test]
    fn temporary_directory_is_removed() {
        let path = {
            let directory = TempDir::new("llxprt-xtask-test").expect("create temporary directory");
            directory.path().to_path_buf()
        };
        assert!(!path.exists());
    }

    #[test]
    fn vendor_lockfiles_matches_bash_glob_hidden_directory_semantics() {
        let root =
            TempDir::new("llxprt-xtask-vendor-lockfiles").expect("create temporary root directory");
        for directory in ["vendor/visible", "vendor/.hidden"] {
            let directory = root.path().join(directory);
            fs::create_dir_all(&directory).expect("create vendor directory");
            fs::write(directory.join("Cargo.lock"), b"").expect("write vendor lockfile");
        }

        assert_eq!(
            vendor_lockfiles(root.path()).expect("discover vendor lockfiles"),
            [PathBuf::from("vendor/visible/Cargo.lock")]
        );
    }

    #[test]
    fn source_bundle_usage_names_every_operation() {
        let usage = source_bundle_usage();
        for operation in ["build", "list", "verify", "test"] {
            assert!(usage.contains(operation));
        }
    }
}
