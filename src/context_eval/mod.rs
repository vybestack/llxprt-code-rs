//! Phase 0 context-management eval harness (issue #37).
//!
//! This module drives the runner-neutral manifests under `evals/context-management/`
//! against a runner adapter and grades the result against independent evidence. It reuses
//! `src/harness.rs` subprocess capture, strict single-JSON parsing, unique sessions, and
//! create-only artifact publication; it never touches `llxprt-parity`'s four coding
//! scenarios or its grader.
//!
//! Expected-red semantics are first class: [`grader::Verdict`] distinguishes a clean,
//! predicted failure of the acceptance target (`ExpectedRed`) from an infrastructure
//! failure (`HarnessError`) and from a red scenario that unexpectedly passed
//! (`UnexpectedGreen`), which is exactly the false-success shape the harness must catch.

pub mod grader;
pub mod loopback;
pub mod manifest;
pub mod report;
pub mod runner;

#[cfg(test)]
mod tests;

use crate::harness::{self, BbResult, ContinuationState, InvocationSpec};
use crate::process::{self, CmdSpec};
use grader::{Evidence, Graded, Verdict};
use loopback::Loopback;
use manifest::Scenario;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Cap on one raw stream copied into an artifact file; the flag records the cut.
pub const ARTIFACT_STREAM_CAP: usize = 64 * 1024;
/// Per-turn subprocess deadline.
pub const TURN_TIMEOUT_SECS: u64 = 600;
/// Default artifact root, repository-local and never a bare `/tmp` path.
pub const DEFAULT_OUT_ROOT: &str = "tmp/issue37-context-evals";

/// Which runner adapter a drive uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunnerKind {
    /// The compiled `llxprt-code-rs` binary: the acceptance target.
    Rust,
    /// The sibling TypeScript implementation through Bun: calibration only, never an oracle.
    Typescript,
}

impl RunnerKind {
    /// Stable name for reports.
    pub fn name(self) -> &'static str {
        match self {
            RunnerKind::Rust => "rust",
            RunnerKind::Typescript => "typescript-reference",
        }
    }
}

/// One harness drive.
#[derive(Clone)]
pub struct Options {
    pub eval_root: PathBuf,
    pub out_root: PathBuf,
    pub runner: RunnerKind,
    pub cli: PathBuf,
    pub ts_root: PathBuf,
    pub ts_bin: String,
    pub allow: Vec<String>,
}

impl Options {
    /// Scenario and fixture directories under one eval root.
    pub fn scenario_dir(&self) -> PathBuf {
        self.eval_root.join("scenarios")
    }

    /// Fixture directory under one eval root.
    pub fn fixtures_dir(&self) -> PathBuf {
        self.eval_root.join("fixtures")
    }
}

/// Resolve the out root to an absolute, existing directory.
///
/// Every path handed to a child process (config home, generated profile, workspace,
/// bulk fixtures, isolated settings) is derived from this absolute form, because the CLI
/// contract requires `LLXPRT_CONFIG_HOME` to name a nonempty absolute directory and a
/// relative `--out` would otherwise leak a relative path into the child environment.
/// Artifacts stay repository-local: only the representation becomes absolute.
fn absolute_out_root(root: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(root).map_err(|e| format!("create out root {}: {e}", root.display()))?;
    let absolute = if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| format!("current dir: {e}"))?
            .join(root)
    };
    absolute
        .canonicalize()
        .map_err(|e| format!("canonicalize {}: {e}", absolute.display()))
}

/// Build an absolute child path under an absolute parent.
///
/// This is the invariant every child-facing path must satisfy: a relative parent here
/// means the out root was never canonicalized, which is a harness bug rather than a
/// scenario result.
fn absolute_child(parent: &Path, name: &str) -> Result<PathBuf, String> {
    let child = parent.join(name);
    if child.is_absolute() {
        Ok(child)
    } else {
        Err(format!(
            "harness path bug: {} is not absolute (out root was not canonicalized)",
            child.display()
        ))
    }
}

/// Load every scenario manifest under the eval root, schema-validated.
pub fn load_scenarios(opts: &Options) -> Result<Vec<(PathBuf, Scenario)>, String> {
    let all = manifest::load_dir(&opts.scenario_dir(), &opts.fixtures_dir())?;
    if opts.allow.is_empty() {
        return Ok(all);
    }
    let chosen: Vec<_> = all
        .into_iter()
        .filter(|(_, scen)| opts.allow.iter().any(|id| id == &scen.id))
        .collect();
    if chosen.is_empty() {
        return Err(format!(
            "none of the requested scenarios {} exist",
            opts.allow.join(", ")
        ));
    }
    Ok(chosen)
}

/// Run every selected scenario and return `(report, all_accepted)`.
pub fn run_all(root: &Path, opts: &Options) -> Result<(Value, bool), String> {
    let opts = &Options {
        out_root: absolute_out_root(&opts.out_root)?,
        ..opts.clone()
    };
    fs::create_dir_all(&opts.out_root).map_err(|e| format!("create out root: {e}"))?;
    let scenarios = load_scenarios(opts)?;
    let revision = git_revision(root);
    let mut reports = Vec::new();
    let mut all_accepted = true;
    for (path, scen) in scenarios {
        let report = run_one(&path, &scen, opts, &revision)?;
        if !accepted(&report) {
            all_accepted = false;
        }
        reports.push(report);
    }
    let report = aggregate(reports, opts, &revision);
    let path = opts
        .out_root
        .join(format!("report-{}.json", harness::uniq()));
    let bytes = serde_json::to_vec(&report).map_err(|e| format!("encode report: {e}"))?;
    publish(&path, &bytes)?;
    harness::eprint_status(&format!("context-evals report: {}", path.display()));
    Ok((report, all_accepted))
}

fn accepted(report: &Value) -> bool {
    matches!(
        report["result"]["verdict"].as_str(),
        Some("pass") | Some("expected-red")
    )
}

fn aggregate(reports: Vec<Value>, opts: &Options, revision: &str) -> Value {
    let mut summary = json!({
        "total": reports.len(),
        "expected_red": 0,
        "passed": 0,
        "unexpected_green": 0,
        "unexpected_red": 0,
        "harness_error": 0,
    });
    for report in &reports {
        let key = match report["result"]["verdict"].as_str() {
            Some("expected-red") => "expected_red",
            Some("pass") => "passed",
            Some("unexpected-green") => "unexpected_green",
            Some("unexpected-red-reason") => "unexpected_red",
            _ => "harness_error",
        };
        *summary.get_mut(key).unwrap_or(&mut json!(0)) =
            json!(summary.get(key).and_then(Value::as_u64).unwrap_or(0) + 1);
    }
    json!({
        "tool": "llxprt-context-eval",
        "schema_version": report::REPORT_SCHEMA_VERSION,
        "run_id": harness::uniq(),
        "runner": opts.runner.name(),
        "runner_revision": revision,
        "expected_status_mode": true,
        "cache": report::cache_block(),
        "summary": summary,
        "scenarios": reports,
    })
}

fn run_one(path: &Path, scen: &Scenario, opts: &Options, revision: &str) -> Result<Value, String> {
    harness::eprint_status(&format!("== context-eval {} ==", scen.id));
    let (evidence, graded, verdict, digests, isolation_ok) = match opts.runner {
        RunnerKind::Rust => drive_rust(scen, opts)?,
        RunnerKind::Typescript => drive_typescript(scen, opts)?,
    };
    let _ = path;
    Ok(json!({
        "id": scen.id,
        "schema_version": report::REPORT_SCHEMA_VERSION,
        "owner_phase": scen.owner_phase,
        "arm": format!("{:?}", scen.arm).to_lowercase(),
        "expected_status": format!("{:?}", scen.expected_status).to_lowercase(),
        "runner": opts.runner.name(),
        "runner_revision": revision,
        "fixture_digests": digests,
        "profile": {
            "name": scen.profile.name,
            "provider": scen.profile.provider,
            "model": scen.profile.model,
            "context_limit_tokens": scen.profile.context_limit_tokens,
            "max_output_tokens": scen.profile.max_output_tokens,
        },
        "result": {
            "verdict": verdict.name(),
            "accepted": verdict.accepted(),
            "reason_class": graded.reason_class,
            "failures": graded.failures,
        },
        "evidence_status": {
            "source": "independent",
            "turns_total": evidence.turns_total,
            "turns_ok": evidence.turns_ok,
            "provider_requests": evidence.provider_requests,
            "tool_calls_scripted": evidence.tool_calls_scripted,
            "final_response_issued": evidence.final_response_issued,
            "wall_hit": evidence.context_limit_hit,
            "terminal_outcome": evidence.terminal_outcome,
            "isolation_ok": isolation_ok,
        },
        "cache": report::cache_block(),
    }))
}

type Drive = (Evidence, Graded, Verdict, Vec<String>, bool);

/// Drive the Rust acceptance target: isolated config home, temporary workspace, generated
/// loopback profile, unique session, one subprocess per turn.
fn drive_rust(scen: &Scenario, opts: &Options) -> Result<Drive, String> {
    let out_dir = opts
        .out_root
        .join(format!("{}-{}", scen.id, harness::uniq()));
    let marker = scen
        .assertions
        .required_final_marker
        .clone()
        .unwrap_or_else(|| "CTXEVAL-FINAL".to_string());
    let rounds = scen.wall.tool_rounds as usize;
    // Bind the port first so the generated profile can point at it, but keep the script
    // empty until the bulk fixtures exist inside the prepared workspace.
    let server = Loopback::start(rounds, Vec::new(), &marker, scen.wall.tool_output_bytes);
    let url = server.base_url();
    let prepared = runner::prepare(&opts.out_root, scen, &url, Vec::new(), Vec::new())?;
    let (bulk, digests) = runner::expand_fixture(
        &opts.fixtures_dir(),
        &scen.wall.fixture,
        scen.wall.tool_rounds,
        scen.wall.tool_output_bytes,
        &prepared.workspace.join("bulk"),
    )?;
    server.set_bulk(bulk.clone());
    let turns = drive_cli_turns(scen, opts, &prepared, &out_dir);
    let obs = server.snapshot();
    server.stop();
    let mut evidence = turns?.0;
    evidence.provider_requests = obs.requests.len();
    evidence.tool_calls_scripted = obs.tool_calls_issued;
    evidence.final_response_issued = obs.final_response_issued;
    evidence.session_dir = session_dir(&prepared);
    let graded = grader::grade(scen, &evidence);
    let verdict = grader::verdict(scen, &graded);
    let isolation_ok = prepared.config_home.starts_with(&opts.out_root)
        && prepared.config_home.is_absolute()
        && prepared.workspace.is_absolute()
        && bulk.iter().all(|p| p.starts_with(&prepared.workspace));
    Ok((evidence, graded, verdict, digests, isolation_ok))
}

/// Drive every prompt of a scenario through the real CLI, one subprocess per turn.
///
/// The acceptance-target binary is selected through `LLXPRT_CODE_RS_BIN` (the same
/// selector `harness::cli_binary` honours), so this harness never guesses a path.
fn drive_cli_turns(
    scen: &Scenario,
    opts: &Options,
    prepared: &runner::Prepared,
    out_dir: &Path,
) -> Result<(Evidence, Vec<BbResult>), String> {
    std::env::set_var("LLXPRT_CODE_RS_BIN", &opts.cli);
    std::env::set_var("LLXPRT_CONFIG_HOME", &prepared.config_home);
    let mut state = ContinuationState::default();
    let mut results = Vec::new();
    for (index, prompt) in scen.prompts().iter().enumerate() {
        let spec = InvocationSpec {
            session: prepared.session.clone(),
            cwd: prepared.workspace.clone(),
            prompt: (*prompt).to_string(),
            turn: if index == 0 {
                None
            } else {
                Some(index as u32 + 1)
            },
            branch: None,
            profile: Some(prepared.profile_name.clone()),
            allow_insecure_http: true,
            allow_shell: false,
        };
        let result = harness::run_cli_with_state(spec, &mut state);
        save_turn_artifacts(out_dir, index, &result)?;
        results.push(result);
        if !results.last().map(|r| r.ok).unwrap_or(false) {
            break;
        }
    }
    Ok((evidence_from_results(&results), results))
}

fn session_dir(prepared: &runner::Prepared) -> Option<PathBuf> {
    let dir = prepared
        .config_home
        .join("code-rs-sessions")
        .join(&prepared.session);
    dir.is_dir().then_some(dir)
}

fn evidence_from_results(results: &[BbResult]) -> Evidence {
    let mut evidence = Evidence {
        turns_total: results.len(),
        ..Evidence::default()
    };
    for result in results {
        if is_harness_error(result) {
            evidence.harness_error = true;
        }
        if result.error_code.contains("context") || result.error_message.contains("context") {
            evidence.context_limit_hit = true;
        }
        if result.ok {
            evidence.turns_ok += 1;
            evidence.last_ok_summary = result.summary.clone();
            evidence.last_ok_stdout = result.raw_stdout.clone();
        }
    }
    evidence
}

fn is_harness_error(result: &BbResult) -> bool {
    result.status == "spawn-failed"
        || result.status == "stdout-contract-broken"
        || result.timed_out
        || result.exit.is_none()
        || result.stdout_truncated
}

/// Drive the TypeScript reference runner. Calibration only: its verdict is reported and
/// never gates the phase, because the sibling implementation is not the oracle.
fn drive_typescript(scen: &Scenario, opts: &Options) -> Result<Drive, String> {
    let out_dir = opts
        .out_root
        .join(format!("{}-{}", scen.id, harness::uniq()));
    let (bulk, digests) = runner::expand_fixture(
        &opts.fixtures_dir(),
        &scen.wall.fixture,
        scen.wall.tool_rounds,
        scen.wall.tool_output_bytes,
        &out_dir.join("bulk"),
    )?;
    let marker = scen
        .assertions
        .required_final_marker
        .clone()
        .unwrap_or_else(|| "CTXEVAL-FINAL".to_string());
    let server = Loopback::start(bulk.len(), bulk, &marker, scen.wall.tool_output_bytes);
    let url = server.base_url();
    let settings = absolute_child(&out_dir, "settings")?;
    fs::create_dir_all(&settings).map_err(|e| format!("create settings: {e}"))?;
    let mut evidence = Evidence {
        turns_total: 1,
        ..Evidence::default()
    };
    let args = runner::ts_args(&scen.stimulus.prompt, &url, &scen.profile.model);
    let outcome = process::run_cmd(CmdSpec {
        program: opts.ts_bin.clone(),
        args,
        cwd: Some(opts.ts_root.clone()),
        cwd_fd: None,
        env_add: vec![
            ("LLXPRT_CONFIG_HOME".into(), settings.display().to_string()),
            ("XDG_CONFIG_HOME".into(), settings.display().to_string()),
            ("CTXEVAL_LOOPBACK_BASE_URL".into(), url),
        ],
        timeout: Duration::from_secs(TURN_TIMEOUT_SECS),
        max_output: 32 * 1024 * 1024,
    });
    let obs = server.snapshot();
    server.stop();
    evidence.provider_requests = obs.requests.len();
    evidence.tool_calls_scripted = obs.tool_calls_issued;
    evidence.final_response_issued = obs.final_response_issued;
    let text = match &outcome {
        Ok(o) => format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
        Err(_) => String::new(),
    };
    match outcome {
        Ok(o) => {
            if o.timed_out || o.status.is_none() {
                evidence.harness_error = true;
            }
            if o.status == Some(0) && serde_json::from_slice::<Value>(&o.stdout).is_ok() {
                evidence.turns_ok = 1;
                evidence.last_ok_stdout = o.stdout.clone();
                evidence.last_ok_summary = String::from_utf8_lossy(&o.stdout).to_string();
            }
            save_ts_artifacts(&out_dir, &o.stdout, &o.stderr)?;
        }
        Err(e) => {
            evidence.harness_error = true;
            harness::eprint_status(&format!("typescript reference spawn failed: {e}"));
        }
    }
    evidence.context_limit_hit =
        text.to_lowercase().contains("context") || text.to_lowercase().contains("compress");
    let graded = grader::grade(scen, &evidence);
    let verdict = grader::verdict(scen, &graded);
    Ok((evidence, graded, verdict, digests, true))
}

/// Publish one turn's bounded raw streams as create-only artifacts.
fn save_turn_artifacts(out_dir: &Path, index: usize, result: &BbResult) -> Result<(), String> {
    fs::create_dir_all(out_dir).map_err(|e| format!("create {}: {e}", out_dir.display()))?;
    let stdout = bounded(result.raw_stdout.as_slice());
    let stderr = bounded(result.stderr.as_slice());
    publish(&out_dir.join(format!("turn-{index:02}.stdout")), &stdout)?;
    publish(&out_dir.join(format!("turn-{index:02}.stderr")), &stderr)?;
    publish(
        &out_dir.join(format!("turn-{index:02}.meta.json")),
        serde_json::to_vec(&json!({
            "status": result.status, "ok": result.ok, "exit": result.exit,
            "timed_out": result.timed_out, "error_code": result.error_code,
            "stdout_truncated": result.stdout_truncated || result.raw_stdout.len() > ARTIFACT_STREAM_CAP,
            "stderr_truncated": result.stderr_truncated || result.stderr.len() > ARTIFACT_STREAM_CAP,
        }))
        .map_err(|e| format!("encode meta: {e}"))?
        .as_slice(),
    )
}

fn save_ts_artifacts(out_dir: &Path, stdout: &[u8], stderr: &[u8]) -> Result<(), String> {
    fs::create_dir_all(out_dir).map_err(|e| format!("create {}: {e}", out_dir.display()))?;
    publish(&out_dir.join("ts.stdout"), &bounded(stdout))?;
    publish(&out_dir.join("ts.stderr"), &bounded(stderr))?;
    Ok(())
}

fn bounded(bytes: &[u8]) -> Vec<u8> {
    bytes[..bytes.len().min(ARTIFACT_STREAM_CAP)].to_vec()
}

fn publish(path: &Path, bytes: &[u8]) -> Result<(), String> {
    match harness::publish_create_only_file(path, bytes) {
        Ok(()) => Ok(()),
        Err(e) => Err(format!("publish {}: {e:?}", path.display())),
    }
}

fn git_revision(root: &Path) -> String {
    std::process::Command::new("git")
        .arg("rev-parse")
        .arg("HEAD")
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}
