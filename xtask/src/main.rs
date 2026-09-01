use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use xtask::release::{run_release_fixtures, run_release_gates, run_source_bundle};
use xtask::{run_gate, Gate};

fn main() -> ExitCode {
    match real_main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn real_main() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let command = args.next().ok_or_else(usage)?;
    let remaining: Vec<String> = args.collect();
    let root = project_root()?;
    match command.as_str() {
        "loc" => no_args(&remaining).and_then(|()| run_gate(&root, Gate::Loc)),
        "complexity" => no_args(&remaining).and_then(|()| run_gate(&root, Gate::Complexity)),
        "quality" => no_args(&remaining).and_then(|()| run_gate(&root, Gate::All)),
        "lint" => no_args(&remaining).and_then(|()| run_lint(&root)),
        "release-gates" => no_args(&remaining).and_then(|()| run_release_gates(&root)),
        "envelope-schema" => run_envelope_schema(&root, &remaining),
        "release-fixtures" => no_args(&remaining).and_then(|()| run_release_fixtures(&root)),
        "source-bundle" => run_source_bundle(&root, &remaining),
        _ => Err(usage()),
    }
}

fn run_envelope_schema(root: &Path, args: &[String]) -> Result<(), String> {
    let check = match args {
        [] => false,
        [arg] if arg == "--check" => true,
        _ => return Err(usage()),
    };
    let output = Command::new("cargo")
        .args([
            "run",
            "--offline",
            "--locked",
            "--quiet",
            "--example",
            "envelope-schema",
        ])
        .current_dir(root)
        .output()
        .map_err(|e| format!("run envelope schema example: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "envelope schema generation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let path = root.join("docs/envelope.schema.json");
    if check {
        check_schema_bytes(&path, &output.stdout)?;
        println!("envelope schema is current");
    } else {
        std::fs::create_dir_all(root.join("docs")).map_err(|e| format!("create docs: {e}"))?;
        std::fs::write(&path, output.stdout)
            .map_err(|e| format!("write {}: {e}", path.display()))?;
        println!("wrote {}", path.display());
    }
    Ok(())
}

fn check_schema_bytes(path: &Path, generated: &[u8]) -> Result<(), String> {
    let tracked = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    if tracked == generated {
        Ok(())
    } else {
        Err("docs/envelope.schema.json is stale; run `cargo xtask envelope-schema`".into())
    }
}

fn no_args(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(usage())
    }
}

fn usage() -> String {
    "usage: cargo xtask <lint|quality|loc|complexity|envelope-schema [--check]|release-gates|release-fixtures|source-bundle>"
        .to_string()
}

fn project_root() -> Result<PathBuf, String> {
    let mut dir = env::current_dir().map_err(|e| format!("current directory: {e}"))?;
    loop {
        if dir.join("xtask/Cargo.toml").is_file() && dir.join("src").is_dir() {
            return Ok(dir);
        }
        if !dir.pop() {
            return Err("run cargo xtask inside the llxprt-code-rs workspace".to_string());
        }
    }
}

fn run_lint(root: &Path) -> Result<(), String> {
    run_command(root, "cargo", &["fmt", "--all", "--", "--check"])?;
    run_command(
        root,
        "cargo",
        &[
            "fmt",
            "--all",
            "--manifest-path",
            "xtask/Cargo.toml",
            "--",
            "--check",
        ],
    )?;
    run_command(
        root,
        "cargo",
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
    run_command(
        root,
        "cargo",
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
    run_gate(root, Gate::All)
}

fn run_command(root: &Path, program: &str, args: &[&str]) -> Result<(), String> {
    let status = Command::new(program)
        .args(args)
        .current_dir(root)
        .status()
        .map_err(|e| format!("run {program}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} {} failed with {status}", args.join(" ")))
    }
}

#[cfg(test)]
mod schema_tests {
    use super::*;

    #[test]
    fn check_accepts_current_and_rejects_corrupt_without_rewriting() {
        let path = std::env::temp_dir().join(format!(
            "llxprt-envelope-schema-check-{}",
            std::process::id()
        ));
        std::fs::write(&path, b"current\n").unwrap();
        assert!(check_schema_bytes(&path, b"current\n").is_ok());
        assert!(check_schema_bytes(&path, b"generated\n").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"current\n");
        std::fs::remove_file(path).unwrap();
    }
}
