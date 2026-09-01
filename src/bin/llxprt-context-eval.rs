//! `llxprt-context-eval`: Phase 0 context-management eval driver (#37).
//!
//! Runs the runner-neutral scenarios under `evals/context-management/` against a runner
//! adapter and publishes one bounded, versioned JSON report. Exit status reflects
//! expected-red semantics: 0 only when every scenario matches its expected status and no
//! harness self-check failed, 1 when a scenario is unexpectedly green, unexpectedly red
//! for another reason, or the harness itself errored, 2 for a usage/schema error.

use clap::Parser;
use llxprt_code_rs::context_eval::{self, Options, RunnerKind, DEFAULT_OUT_ROOT};
use std::path::PathBuf;
use std::process::ExitCode;

/// CLI for the context eval driver.
#[derive(Debug, Parser)]
#[command(
    name = "llxprt-context-eval",
    version,
    about = "Context-management eval harness."
)]
struct Args {
    /// Compiled llxprt-code-rs binary (the Rust acceptance target).
    #[arg(long, default_value = "target/debug/llxprt-code-rs")]
    cli: PathBuf,

    /// Eval root holding scenarios/ and fixtures/.
    #[arg(long, default_value = "evals/context-management")]
    eval_root: PathBuf,

    /// Artifact root (repository-local, never a bare /tmp path).
    #[arg(long, default_value = DEFAULT_OUT_ROOT)]
    out: PathBuf,

    /// Runner adapter: rust (acceptance target) or ts (calibration reference).
    #[arg(long, default_value = "rust")]
    runner: String,

    /// Comma-separated scenario allow-list.
    #[arg(long)]
    scenarios: Option<String>,

    /// Sibling TypeScript repository root for the reference runner.
    #[arg(long, default_value = context_eval::runner::TS_ROOT_DEFAULT)]
    ts_root: PathBuf,

    /// Binary used to start the TypeScript reference runner.
    #[arg(long, default_value = "bun")]
    ts_bin: String,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let runner = match args.runner.as_str() {
        "rust" => RunnerKind::Rust,
        "ts" | "typescript" => RunnerKind::Typescript,
        other => {
            eprintln!("unknown --runner {other} (expected rust or ts)");
            return ExitCode::from(2);
        }
    };
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let options = Options {
        eval_root: args.eval_root.clone(),
        out_root: args.out.clone(),
        runner,
        cli: args.cli.clone(),
        ts_root: args.ts_root.clone(),
        ts_bin: args.ts_bin.clone(),
        allow: args
            .scenarios
            .map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
            .unwrap_or_default(),
    };
    match context_eval::run_all(&root, &options) {
        Ok((report, all_accepted)) => {
            print!("{}", report);
            if all_accepted {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("context-evals harness error: {error}");
            ExitCode::from(2)
        }
    }
}
