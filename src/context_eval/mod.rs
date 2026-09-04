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

pub mod faults;
pub mod grader;
pub mod inject;
pub mod loopback;
pub mod manifest;
pub mod records;
pub mod report;
pub mod runner;
pub mod secrets;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_records;

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
    /// Root holding the append-only expected-status history and observed-run records.
    /// Defaults to `<eval_root>/records` when left empty.
    pub records_root: PathBuf,
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
    let store = records::Records::new(&opts.records_root);
    let history = records::load_history(&store.history_path())?;
    for (_, scen) in &scenarios {
        records::validate_history(
            &history,
            &scen.id,
            scen.owner_phase,
            scen.expected_status.name(),
            &scen.expected_reason_class,
        )
        .map_err(|e| format!("scenario {}: {e}", scen.id))?;
    }
    // Append-only phase-indexed history (R-013): a manifest's own declaration for its
    // owning phase is recorded as a new entry the first time this harness sees it, so
    // every expectation this run graded against is preserved with the phase that owned
    // it. An already-recorded phase is left untouched, which is what makes the file
    // append-only rather than rewritten.
    for (_, scen) in &scenarios {
        if !records::has_phase_entry(&history, &scen.id, scen.owner_phase) {
            store
                .record(&records::StatusRecord {
                    scenario: scen.id.clone(),
                    phase: scen.owner_phase,
                    expected_status: scen.expected_status.name().to_string(),
                    expected_reason_class: scen.expected_reason_class.clone(),
                    accept_any_reason: scen.accept_any_reason,
                    note: format!(
                        "phase {} declared expectation for scenario {}",
                        scen.owner_phase, scen.id
                    ),
                })
                .map_err(|e| format!("scenario {}: {e}", scen.id))?;
        }
    }
    let revision = git_revision(root);
    let mut reports = Vec::new();
    let mut all_accepted = true;
    for (path, scen) in scenarios {
        let report = run_one(&path, &scen, opts, &revision, &store)?;
        if !accepted(&report) {
            all_accepted = false;
        }
        reports.push(report);
    }
    let report = aggregate(reports, opts, &revision);
    // The publish path validates what this run actually produced, so a harness change
    // that drops a required field fails here instead of publishing a report the schema
    // would reject. Validating before the write keeps a bad report unpublishable.
    report::validate(&report, true)
        .map_err(|e| format!("aggregate report failed schema validation: {e}"))?;
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
    let store = records::Records::new(&opts.records_root);
    let ids: Vec<String> = reports
        .iter()
        .filter_map(|r| r["id"].as_str().map(str::to_string))
        .collect();
    let baseline = records::baseline_summary(&store, &ids).unwrap_or(json!(null));
    json!({
        "tool": "llxprt-context-eval",
        "schema_version": report::REPORT_SCHEMA_VERSION,
        "run_id": harness::uniq(),
        "runner": opts.runner.name(),
        "runner_revision": revision,
        "expected_status_mode": true,
        "phase0_baseline": baseline,
        "records_root": opts.records_root.display().to_string(),
        "cache": report::aggregate_cache(&reports),
        "summary": summary,
        "scenarios": reports,
    })
}

fn run_one(
    path: &Path,
    scen: &Scenario,
    opts: &Options,
    revision: &str,
    store: &records::Records,
) -> Result<Value, String> {
    harness::eprint_status(&format!("== context-eval {} ==", scen.id));
    let (evidence, graded, verdict, digests, isolation_ok) = match opts.runner {
        RunnerKind::Rust => drive_rust(scen, opts)?,
        RunnerKind::Typescript => drive_typescript(scen, opts)?,
    };
    let _ = path;
    let dims = grader::dimension_results(scen, &evidence);
    let evidence_dimensions = dimensions_block(&dims);
    let request_observations = request_observations_block(&evidence);
    // Captured before the move below: the run record needs the verdict name too.
    let verdict_name = verdict.name().to_string();
    let observed_status = if graded.passed { "green" } else { "red" }.to_string();
    let observed_reason = graded.reason_class.clone();
    let session_dir = evidence.session_dir.clone();
    let drive = DriveOutcome::new(
        evidence,
        graded,
        verdict,
        digests,
        isolation_ok,
        session_dir.as_deref(),
    );
    let report = scenario_report(
        scen,
        opts,
        revision,
        &drive,
        evidence_dimensions,
        request_observations,
    );

    // Validate the report this drive produced before it leaves the harness: this is the
    // publish-path check, exercised on real run output rather than a fixture of itself.
    report::validate(&report, false).map_err(|e| {
        format!(
            "scenario {id} report failed schema validation: {e}",
            id = scen.id
        )
    })?;

    // R-013: every observation lands in the append-only run record for this phase, so
    // the red-then-green trail survives later phases turning scenarios green.
    store.record_run(&records::RunRecord {
        scenario: scen.id.clone(),
        phase: scen.owner_phase,
        observed_status,
        reason_class: observed_reason,
        verdict: verdict_name,
        report: opts.out_root.display().to_string(),
    })?;

    Ok(report)
}

/// The evidence-dimensions block: one boolean per dimension plus the failures charged
/// to each, so a report reader sees which dimension failed and why (R-016, GAP-M15).
fn dimensions_block(dims: &[(grader::Dimension, bool, Vec<String>)]) -> Value {
    let dim_pass = |field: &str| dims.iter().any(|(d, ok, _)| d.field() == field && *ok);
    let dim_failures: Vec<String> = dims
        .iter()
        .flat_map(|(d, _, failures)| {
            failures
                .iter()
                .map(|f| format!("[{}] {f}", d.field()))
                .collect::<Vec<String>>()
        })
        .collect();
    json!({
        "task": dim_pass("task"),
        "protocol": dim_pass("protocol"),
        "resource": dim_pass("resource"),
        "latency": dim_pass("latency"),
        "recovery": dim_pass("recovery"),
        "wall_realism": dim_pass("wall_realism"),
        "failures": dim_failures,
    })
}

/// The loopback-observed request telemetry block (GAP-M17).
fn request_observations_block(evidence: &Evidence) -> Value {
    json!({
        "requests": evidence.provider_requests,
        "max_request_bytes": evidence.max_request_bytes,
        "streamed_requests": evidence.streamed_requests,
        "tool_names": evidence.tool_names,
        "last_request_bytes": evidence.last_request_bytes,
        "observations_source": "loopback",
        "serialized": evidence.request_bodies_digest,
    })
}

/// One graded drive, carried together so the report assembly takes one value instead of
/// a long positional list.
struct DriveOutcome {
    evidence: Evidence,
    graded: Graded,
    verdict: Verdict,
    digests: Vec<String>,
    isolation_ok: bool,
    cache: Value,
}

impl DriveOutcome {
    /// Bundle a completed drive with its derived cache block.
    fn new(
        evidence: Evidence,
        graded: Graded,
        verdict: Verdict,
        digests: Vec<String>,
        isolation_ok: bool,
        session_dir: Option<&Path>,
    ) -> Self {
        let cache = report::cache_block_from_session(session_dir);
        Self {
            evidence,
            graded,
            verdict,
            digests,
            isolation_ok,
            cache,
        }
    }
}

/// Assemble one scenario report from the graded drive, carrying every field the
/// publish-path schema requires.
fn scenario_report(
    scen: &Scenario,
    opts: &Options,
    revision: &str,
    drive: &DriveOutcome,
    evidence_dimensions: Value,
    request_observations: Value,
) -> Value {
    let (evidence, graded, verdict) = (&drive.evidence, &drive.graded, &drive.verdict);
    json!({
        "id": scen.id,
        "schema_version": report::REPORT_SCHEMA_VERSION,
        "owner_phase": scen.owner_phase,
        "arm": scen.arm.name(),
        "expected_status": scen.expected_status.name(),
        "runner": opts.runner.name(),
        "runner_revision": revision,
        "fixture_digests": drive.digests,
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
            "isolation_ok": drive.isolation_ok,
        },
        "cache": drive.cache,
        "runtime_config": {
            "name": scen.runtime.name,
            "context_limit": scen.runtime.context_limit,
        },
        "evidence_dimensions": evidence_dimensions,
        "request_observations": request_observations,
        "leakage_scan": {
            "clean": evidence.leaks.is_empty(),
            "findings": evidence
                .leaks
                .iter()
                .map(|(marker, found_in)| json!({ "marker": marker, "found_in": found_in }))
                .collect::<Vec<Value>>(),
            "markers": secrets::LEAK_MARKERS.iter().map(|m| json!(m)).collect::<Vec<Value>>(),
        },
    })
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
    let shared_observations = server.observations_handle();
    let prepared = runner::prepare(&opts.out_root, scen, &url, Vec::new(), Vec::new())?;
    let (bulk, digests) = runner::expand_fixture(
        &opts.fixtures_dir(),
        &scen.wall.fixture,
        scen.wall.tool_rounds,
        scen.wall.tool_output_bytes,
        &prepared.workspace.join("bulk"),
    )?;
    server.set_bulk(bulk.clone());
    let (mut evidence, turn_results) =
        drive_cli_turns(scen, opts, &prepared, &out_dir, shared_observations)?;
    let obs = server.snapshot();
    server.stop();
    evidence_from_loopback(&mut evidence, &obs);
    evidence.session_dir = session_dir(&prepared);
    scan_drive_outputs(&mut evidence, &prepared, &out_dir, &turn_results);
    let graded = grader::grade(scen, &evidence);
    let verdict = grader::verdict(scen, &graded);
    let isolation_ok = prepared.config_home.starts_with(&opts.out_root)
        && prepared.config_home.is_absolute()
        && prepared.workspace.is_absolute()
        && bulk.iter().all(|p| p.starts_with(&prepared.workspace));
    Ok((evidence, graded, verdict, digests, isolation_ok))
}

/// Copy the loopback's own observations onto the evidence the grader reads, so the
/// request telemetry in a report is what the server saw, never what the run claimed.
fn evidence_from_loopback(evidence: &mut Evidence, obs: &loopback::Observations) {
    evidence.provider_requests = obs.requests.len();
    evidence.tool_calls_scripted = obs.tool_calls_issued;
    evidence.final_response_issued = obs.final_response_issued;
    // Persisted request observations (GAP-M17): sizes, tools, stream mode, digest.
    evidence.request_bytes = obs.requests.iter().map(|r| r.body_bytes).collect();
    evidence.max_request_bytes = obs.requests.iter().map(|r| r.body_bytes).max().unwrap_or(0);
    evidence.last_request_bytes = obs.requests.last().map(|r| r.body_bytes).unwrap_or(0);
    evidence.streamed_requests = obs.requests.iter().filter(|r| r.streamed).count();
    let mut tool_names: Vec<String> = Vec::new();
    for req in &obs.requests {
        for name in &req.tool_names {
            if !tool_names.contains(name) {
                tool_names.push(name.clone());
            }
        }
    }
    evidence.tool_names = tool_names;
    evidence.request_bodies_digest = hex_digest(
        &obs.requests
            .iter()
            .flat_map(|r| r.body_bytes.to_be_bytes())
            .collect::<Vec<u8>>(),
    );
}

/// Leakage scan (R-012) over everything this run produced or captured: the isolated
/// config home, the workspace, the harness artifacts, and every captured stream. The
/// bulk fixtures are the plant's input and are excluded, so an input can never
/// masquerade as an escaped output.
fn scan_drive_outputs(
    evidence: &mut Evidence,
    prepared: &runner::Prepared,
    out_dir: &Path,
    turn_results: &[BbResult],
) {
    let bulk_dir = prepared.workspace.join("bulk");
    evidence.leaks = secrets::scan_tree_skipping(&prepared.config_home, Some(bulk_dir.as_path()))
        .into_iter()
        .map(|(marker, found)| (marker, format!("config home: {found}")))
        .chain(
            secrets::scan_tree_skipping(&prepared.workspace, Some(bulk_dir.as_path()))
                .into_iter()
                .map(|(marker, found)| (marker, format!("workspace: {found}"))),
        )
        .chain(
            secrets::scan_tree_skipping(out_dir, None)
                .into_iter()
                .map(|(marker, found)| (marker, format!("harness artifacts: {found}"))),
        )
        .collect();
    for result in turn_results {
        for stream in [&result.raw_stdout, &result.stderr] {
            for marker in secrets::scan_bytes(stream) {
                evidence
                    .leaks
                    .push((marker.to_string(), "captured stream".to_string()));
            }
        }
        for marker in secrets::scan_bytes(result.summary.as_bytes()) {
            evidence
                .leaks
                .push((marker.to_string(), "envelope summary".to_string()));
        }
    }
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
    shared_observations: std::sync::Arc<std::sync::Mutex<loopback::Observations>>,
) -> Result<(Evidence, Vec<BbResult>), String> {
    std::env::set_var("LLXPRT_CODE_RS_BIN", &opts.cli);
    std::env::set_var("LLXPRT_CONFIG_HOME", &prepared.config_home);
    // The store fault is scoped by scenario id, never by turn index: this scenario is one
    // invocation, so any turn-anchored chmod would only ever fire after the run that
    // should have observed it had already completed.
    let fault_guard = inject::StoreUnwritableGuard::new(scen, prepared);
    let mut _fault_thread = fault_guard
        .as_ref()
        .map(inject::StoreUnwritableInjection::start);
    let (kill_target, fault_handle) = arm_mid_run_fault(scen, opts, out_dir, shared_observations)?;
    let mut state = ContinuationState::default();
    let results = drive_turns(scen, prepared, out_dir, &mut state)?;
    if let Some(target) = &kill_target {
        target.stop.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    let store_fault_applied = _fault_thread
        .as_mut()
        .map(inject::StoreUnwritableInjection::applied)
        .unwrap_or(false);
    drop(_fault_thread);
    let mut faults_executed: Vec<String> = Vec::new();
    if let Some(handle) = fault_handle {
        if let Some(trigger) = handle.join().ok().flatten() {
            faults_executed.push(trigger);
        }
    }
    let mut evidence = evidence_from_results(&results);
    if store_fault_applied {
        faults_executed.push("context store made unwritable mid-invocation".to_string());
    }
    evidence.faults_executed = faults_executed;
    if evidence.faults_executed.is_empty() {
        return Ok((evidence, results));
    }
    // A fault-triggered SIGKILL kills the bounded runner's child (exit None, no timeout,
    // nothing truncated). That is the fault working, never a harness infrastructure
    // failure; any other broken shape is still infrastructure and stays flagged.
    if !killed_by_infra(&results) {
        evidence.harness_error = false;
    }
    recovery_probe(prepared, out_dir, &mut evidence, &results)?;
    Ok((evidence, results))
}

/// Drive one turn of a scenario through the real CLI, bounded per turn.
fn turn_spec(prepared: &runner::Prepared, index: usize, prompt: &str) -> InvocationSpec {
    InvocationSpec {
        session: prepared.session.clone(),
        cwd: prepared.workspace.clone(),
        prompt: prompt.to_string(),
        turn: if index == 0 {
            None
        } else {
            Some(index as u32 + 1)
        },
        branch: None,
        profile: Some(prepared.profile_name.clone()),
        allow_insecure_http: true,
        allow_shell: false,
    }
}

/// Drive every prompt of a scenario in order, stopping at the first turn that did not
/// complete cleanly, so a killed run is never billed for turns the fault removed.
fn drive_turns(
    scen: &Scenario,
    prepared: &runner::Prepared,
    out_dir: &Path,
    state: &mut ContinuationState,
) -> Result<Vec<BbResult>, String> {
    let mut results = Vec::new();
    for (index, prompt) in scen.prompts().iter().enumerate() {
        let spec = turn_spec(prepared, index, prompt);
        let result = harness::run_cli_with_state(spec, state);
        save_turn_artifacts(out_dir, index, &result)?;
        results.push(result);
        if !results.last().map(|r| r.ok).unwrap_or(false) {
            break;
        }
    }
    Ok(results)
}

/// Whether any result carries a shape only harness infrastructure can produce, i.e. a
/// broken process that was *not* a clean process-group kill from an executed fault.
fn killed_by_infra(results: &[BbResult]) -> bool {
    let killed_by_fault = |r: &BbResult| {
        r.exit.is_none()
            && !r.timed_out
            && !r.stdout_truncated
            && !r.stderr_truncated
            && !r.combined_truncated
            && r.status != "spawn-failed"
    };
    results.iter().any(|r| {
        !killed_by_fault(r)
            && (r.status == "spawn-failed"
                || r.status == "stdout-contract-broken"
                || r.timed_out
                || r.stdout_truncated
                || r.stderr_truncated
                || r.combined_truncated)
    })
}

/// Armed mid-run fault, if the scenario selected one: the kill target to match against
/// results, and the watcher thread that performs the kill and reports what it did.
type ArmedMidRun = (
    Option<inject::KillTarget>,
    Option<std::thread::JoinHandle<Option<String>>>,
);

/// Arm the mid-run process-death fault a scenario selected, if it selected one.
///
/// Delegates to [`inject::arm_mid_run_fault`]; the wrapper exists so the drive reads as
/// one flow while the injection machinery stays a separate module.
fn arm_mid_run_fault(
    scen: &Scenario,
    opts: &Options,
    out_dir: &Path,
    shared_observations: std::sync::Arc<std::sync::Mutex<loopback::Observations>>,
) -> Result<ArmedMidRun, String> {
    let armed = inject::arm_mid_run_fault(scen, &opts.cli, out_dir, shared_observations)?;
    Ok(match armed {
        Some(inject::ArmedFault { target, handle }) => (Some(target), Some(handle)),
        None => (None, None),
    })
}

/// Recovery probe after an executed fault: the killed process is gone, so the next
/// invocation is a real process restart against the same session. A valid envelope of
/// either kind (ok, or a clean typed error such as a context refusal) proves the
/// persisted store reopened and replayed; no envelope means the restart path is broken.
fn recovery_probe(
    prepared: &runner::Prepared,
    out_dir: &Path,
    evidence: &mut Evidence,
    results: &[BbResult],
) -> Result<(), String> {
    let probe_index = results.len();
    let probe = InvocationSpec {
        session: prepared.session.clone(),
        cwd: prepared.workspace.clone(),
        prompt:
            "CTXEVAL-RESTART-PROBE: confirm this session reopened after the restart; reply briefly."
                .to_string(),
        turn: Some(probe_index as u32 + 1),
        branch: None,
        profile: Some(prepared.profile_name.clone()),
        allow_insecure_http: true,
        allow_shell: false,
    };
    let result = harness::run_cli(probe);
    save_turn_artifacts(out_dir, probe_index, &result)?;
    let restarted = result.status == "ok" || result.status == "error";
    evidence.recovery_after_fault = restarted
        && session_dir(prepared)
            .map(|session| inject::store_shape_consistent(&session.join("context")))
            .unwrap_or(false);
    let probe_outcome = if restarted {
        "session reopened with a valid envelope"
    } else {
        "no valid envelope (status mismatch)"
    };
    harness::eprint_status(&format!(
        "context-evals recovery probe after {}: {probe_outcome}",
        evidence.faults_executed.join(", ")
    ));
    Ok(())
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
            evidence.resource_limit_hit = true;
        }
        if result.ok {
            evidence.turns_ok += 1;
            evidence.last_ok_summary = result.summary.clone();
            evidence.last_ok_stdout = result.raw_stdout.clone();
            if let Some(outcome) = declared_terminal_outcome(&result.raw_stdout) {
                evidence.terminal_outcome = Some(outcome);
            }
        }
    }
    evidence
}

/// Observe a terminal outcome the context runtime declared in its stdout summary JSON.
///
/// This is harness observation, never grading: a run that never declares an outcome yields
/// `None`, and a declared-but-false outcome is still only a declaration. The grader keeps
/// its own exact comparison, so nothing here can manufacture a `required_outcomes` pass.
fn declared_terminal_outcome(stdout: &[u8]) -> Option<String> {
    let value = serde_json::from_slice::<Value>(stdout).ok()?;
    let outcome = value.get("terminal_outcome")?.as_str()?;
    (!outcome.is_empty()).then(|| outcome.to_string())
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
    // `Loopback::start` only binds the port; the scripted rounds arrive through
    // `set_bulk` once the fixtures exist, exactly as the Rust drive does. Without the
    // hand-off the stub serves an empty script and answers every turn with the final
    // marker, so no wall is ever exercised.
    let rounds = bulk.len();
    let server = Loopback::start(rounds, Vec::new(), &marker, scen.wall.tool_output_bytes);
    server.set_bulk(bulk);
    let url = server.base_url();
    let settings = absolute_child(&out_dir, "settings")?;
    fs::create_dir_all(&settings).map_err(|e| format!("create settings: {e}"))?;
    let mut evidence = Evidence {
        turns_total: 1,
        ..Evidence::default()
    };
    // The TS reference runner has no fault or recovery machinery; the harness records
    // that honestly rather than simulating an execution.
    if !scen.faults.injected.is_empty() {
        evidence.faults_executed = Vec::new();
    }
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
    let stdout_truncated = result.stdout_truncated || result.raw_stdout.len() > ARTIFACT_STREAM_CAP;
    let stderr_truncated = result.stderr_truncated || result.stderr.len() > ARTIFACT_STREAM_CAP;
    publish(
        &out_dir.join(format!("turn-{index:02}.meta.json")),
        serde_json::to_vec(&json!({
            "status": result.status, "ok": result.ok, "exit": result.exit,
            "timed_out": result.timed_out, "error_code": result.error_code,
            "stdout_truncated": stdout_truncated,
            "stderr_truncated": stderr_truncated,
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

/// Lowercase hex digest of `bytes` (request-body ordering digest).
fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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
