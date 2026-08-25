use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
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
    if args.next().is_some() {
        return Err(usage());
    }
    let root = project_root()?;
    match command.as_str() {
        "loc" => run_gate(&root, Gate::Loc),
        "complexity" => run_gate(&root, Gate::Complexity),
        "quality" => run_gate(&root, Gate::All),
        "lint" => run_lint(&root),
        _ => Err(usage()),
    }
}

fn usage() -> String {
    "usage: cargo xtask <lint|quality|loc|complexity>".to_string()
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
