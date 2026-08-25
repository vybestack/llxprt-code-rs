//! Parity binding for the `llxprt-code-rs` CLI: runs the real binary as a
//! subprocess against a live model, grades the produced workspace, and writes a JSON report.
//!
//! Every CLI turn is a fresh subprocess with exactly one JSON object on its stdout; the raw
//! stdout/stderr and the per-turn artifact files are preserved on disk verbatim
//! (untrimmed). Follow-ups share the same workspace and `--session`, and stop after the
//! first failure. The `dsflash` scenarios pass `--allow-insecure-http` and
//! `--allow-shell` explicitly.
//!
//! `--all` runs the four scenarios; `--scenarios` is an allow-list; `--all`
//! conflicts with `--scenarios`. The report is the single JSON value on stdout; all
//! progress and the report path go to stderr (always, even when stderr is not a
//! terminal). The binary exits nonzero if any requested scenario failed its grader, which
//! includes build, structural, protocol, and hidden-grader evidence, not protocol alone.

use clap::Parser;
use llxprt_code_rs::grade;
use llxprt_code_rs::harness::{self, InvocationSpec};
use serde_json::json;
use std::fs;
use std::path::PathBuf;

/// Scenario names the parity binary knows.
const SCENARIOS: [&str; 4] = ["starter", "pong", "flappy", "encryption"];

/// CLI for the parity harness.
#[derive(Debug, Parser)]
#[command(
    name = "llxprt-parity",
    version,
    about = "Parity harness for the llxprt-code-rs CLI."
)]
struct Args {
    /// Output root for workspace artifacts (default: llxprt-parity-out).
    #[arg(long)]
    out: Option<PathBuf>,

    /// Exact create-only report destination (default: a unique report file under --out).
    #[arg(long)]
    report_path: Option<PathBuf>,

    /// Run all four scenarios (starter, pong, flappy, encryption). Conflicts with --scenarios.
    #[arg(long, conflicts_with = "scenarios")]
    all: bool,

    /// Comma-separated scenario allow-list. Conflicts with --all.
    #[arg(long)]
    scenarios: Option<String>,

    /// Named profile to use (default: dsflash-mi300x).
    #[arg(long)]
    profile: Option<String>,
}

fn main() {
    let args = Args::parse();
    let out_root = args
        .out
        .clone()
        .unwrap_or_else(|| PathBuf::from("llxprt-parity-out"));
    let chosen = chosen_scenarios(&args);
    if let Err(msg) = check_scenarios(&chosen) {
        harness::eprint_status(&msg);
        std::process::exit(2);
    }
    if let Err(e) = fs::create_dir_all(&out_root) {
        harness::eprint_status(&format!("cannot create --out {}: {e}", out_root.display()));
        std::process::exit(3);
    }

    let run_id = harness::uniq();
    let (reports, any_failed) = collect_reports(&chosen, &out_root, args.profile.as_deref());
    let report = json!({
        "tool": "llxprt-parity",
        "run_id": run_id,
        "profile": args.profile.clone().unwrap_or_else(|| "dsflash-mi300x".to_string()),
        "scenarios": reports,
    });
    let report_path = args
        .report_path
        .clone()
        .unwrap_or_else(|| out_root.join(format!("report-{run_id}.json")));
    publish_report(&report_path, &report);
    if any_failed {
        harness::eprint_status("one or more scenarios failed; exiting nonzero");
        std::process::exit(1);
    }
}

fn chosen_scenarios(args: &Args) -> Vec<String> {
    if args.all {
        SCENARIOS.iter().map(|s| s.to_string()).collect()
    } else {
        match &args.scenarios {
            None => vec!["starter".to_string()],
            Some(s) => s.split(',').map(|x| x.trim().to_string()).collect(),
        }
    }
}

fn collect_reports(
    chosen: &[String],
    out_root: &std::path::Path,
    profile: Option<&str>,
) -> (Vec<serde_json::Value>, bool) {
    let mut reports = Vec::new();
    let mut any_failed = false;
    for name in chosen {
        harness::eprint_status(&format!(
            "== scenario {name} (profile {}) ==",
            profile.unwrap_or("dsflash-mi300x")
        ));
        match run_scenario(name, out_root, profile) {
            Ok(mut report) => {
                let failed = scenario_failed(&report);
                if failed {
                    harness::eprint_status(&format!(
                        "scenario {name} failed (protocol/build/structural/hidden)"
                    ));
                }
                any_failed |= failed;
                report["scenario"] = json!(name);
                reports.push(report);
            }
            Err(e) => {
                harness::eprint_status(&format!("scenario {name} failed to start: {e}"));
                any_failed = true;
                reports.push(failed_scenario_report(name, e));
            }
        }
    }
    (reports, any_failed)
}

fn scenario_failed(report: &serde_json::Value) -> bool {
    let passed = report
        .get("question")
        .and_then(|q| q.get("passed"))
        .and_then(|p| p.as_bool())
        .unwrap_or(false);
    !passed || report.get("error").is_some()
}

fn failed_scenario_report(name: &str, error: String) -> serde_json::Value {
    json!({
        "scenario": name,
        "error": error,
        "question": {
            "passed": false,
            "turns_run": 0,
            "workspace": "unavailable".to_string(),
            "scenario": name,
        },
        "scores": {
            "protocol": 0.0,
            "tool_use": 0.0,
            "build_test": 0.0,
            "structural": 0.0,
        },
        "hidden_graders_pass": false,
    })
}

fn publish_report(report_path: &std::path::Path, report: &serde_json::Value) {
    if let Err(e) = report_persist(report_path, &format!("{report}")) {
        let err_report = persist_error_report(report_path, report, &e);
        println!("{err_report}");
        harness::eprint_status(&format!("report persistence incomplete: {e}"));
        std::process::exit(3);
    }
    println!("{report}");
    harness::eprint_status(&format!("report written to {}", report_path.display()));
}

fn persist_error_report(
    report_path: &std::path::Path,
    report: &serde_json::Value,
    error: &ReportPersistError,
) -> serde_json::Value {
    match error {
        ReportPersistError::BeforePublication(source) => json!({
            "tool": "llxprt-parity",
            "status": "error",
            "error": {
                "code": "report-persist",
                "message": format!("cannot write {}: {source}", report_path.display()),
            },
        }),
        ReportPersistError::AfterPublication(source) => json!({
            "tool": "llxprt-parity",
            "status": "error",
            "published": true,
            "durability": "unconfirmed",
            "report": report,
            "error": {
                "code": "report-published-durability-unconfirmed",
                "message": format!("report is visible at {} but durability could not be confirmed: {source}", report_path.display()),
            },
        }),
    }
}

/// Validate the scenario allow-list. Unknown names are a usage error.
fn check_scenarios(chosen: &[String]) -> Result<(), String> {
    if chosen.is_empty() {
        return Err("error: no scenarios requested".into());
    }
    for name in chosen {
        if !SCENARIOS.contains(&name.as_str()) {
            return Err(format!("error: unknown scenario {name}"));
        }
    }
    Ok(())
}

#[derive(Debug)]
enum ReportPersistError {
    BeforePublication(std::io::Error),
    AfterPublication(std::io::Error),
}

impl std::fmt::Display for ReportPersistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BeforePublication(e) => write!(f, "before publication: {e}"),
            Self::AfterPublication(e) => write!(f, "after publication: {e}"),
        }
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReportPersistStage {
    AfterInstall,
}

#[cfg(not(test))]
type ReportPersistStage = std::convert::Infallible;

/// Write report JSON through a retained parent descriptor. The exact synced candidate descriptor
/// is installed create-only, its digest is verified, and the retained directory is synced.
fn report_persist(path: &std::path::Path, value: &str) -> Result<(), ReportPersistError> {
    report_persist_inner(path, value, None)
}

fn report_persist_inner(
    path: &std::path::Path,
    value: &str,
    inject_after: Option<ReportPersistStage>,
) -> Result<(), ReportPersistError> {
    match harness::publish_create_only_file(path, value.as_bytes()) {
        Ok(()) => {}
        Err(harness::ArtifactPublishError::BeforePublication(message)) => {
            return Err(ReportPersistError::BeforePublication(
                std::io::Error::other(message),
            ));
        }
        Err(harness::ArtifactPublishError::InstalledDurabilityUnknown(message)) => {
            return Err(ReportPersistError::AfterPublication(std::io::Error::other(
                message,
            )));
        }
    }
    if let Some(stage) = inject_after {
        return Err(ReportPersistError::AfterPublication(std::io::Error::other(
            format!("injected report persistence failure at {stage:?}"),
        )));
    }
    Ok(())
}

/// Run one scenario, driving every turn through the harness and grading the produced
/// workspace. A **unique** valid session id is generated once per `run_scenario`
/// invocation and shared across that scenario's continuation turns, so separate runs over
/// fresh workspaces never reuse a fixed scenario name; the scenario name stays the report
/// label and the artifact directory (`--out/<scenario>/`).
fn run_scenario(
    name: &str,
    out_root: &std::path::Path,
    profile: Option<&str>,
) -> Result<serde_json::Value, String> {
    let workspace = harness::fresh_workspace(out_root);
    let workspace_cap = llxprt_code_rs::tools::WorkspaceCap::open(&workspace)
        .map_err(|error| format!("open scenario workspace: {error}"))?;
    let scenario = harness::scenarios()
        .into_iter()
        .find(|s| s.name == name)
        .ok_or_else(|| format!("unknown scenario {name}"))?;
    let session_id = format!("{name}-{}", harness::uniq());
    let mut continuation = harness::ContinuationState::default();
    let results = harness::run_turns(&scenario, |prompt, turn| {
        let t = turn.unwrap_or(1);
        let r = harness::run_cli_with_state(
            InvocationSpec {
                session: session_id.clone(),
                cwd: workspace.clone(),
                prompt: prompt.to_string(),
                turn,
                branch: None,
                profile: profile.map(str::to_string),
                allow_insecure_http: true,
                allow_shell: true,
            },
            &mut continuation,
        );
        harness::save_turn(out_root, name, &session_id, t, &r)
            .map_err(|e| format!("failed to save turn artifacts for turn {t}: {e}"))?;
        harness::eprint_status(&format!("  turn {t}: {}", r.status));
        Ok(r)
    })?;
    let report = grade::report_with_cap(name, &workspace, &workspace_cap, &results);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    fn assert_private_stages_are_empty(dir: &std::path::Path) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name().to_string_lossy().starts_with(".stage.") {
                assert_eq!(entry.metadata().unwrap().len(), 0);
            }
        }
    }

    #[test]
    fn report_publication_never_replaces_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report-test.json");
        std::fs::write(&path, b"original").unwrap();
        assert!(report_persist(&path, "replacement").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
        assert_private_stages_are_empty(dir.path());
    }

    #[test]
    fn concurrent_report_publication_has_one_winner() {
        let dir = tempfile::tempdir().unwrap();
        let path = Arc::new(dir.path().join("report-test.json"));
        let barrier = Arc::new(Barrier::new(2));
        let mut threads = Vec::new();
        for value in ["first", "second"] {
            let path = Arc::clone(&path);
            let barrier = Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                (value, report_persist(&path, value))
            }));
        }
        let outcomes = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            outcomes.iter().filter(|(_, result)| result.is_ok()).count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|(_, result)| result.is_err())
                .count(),
            1
        );
        let published = std::fs::read_to_string(&*path).unwrap();
        let is_complete_report = published == "first" || published == "second";
        assert!(is_complete_report);
        assert_private_stages_are_empty(dir.path());
    }

    #[test]
    fn post_publication_failures_are_distinct_and_leave_the_report_visible() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report-test.json");
        let error =
            report_persist_inner(&path, "published", Some(ReportPersistStage::AfterInstall))
                .unwrap_err();
        assert!(matches!(error, ReportPersistError::AfterPublication(_)));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "published");
        assert_private_stages_are_empty(dir.path());
    }
}
