//! Independent grader and expected-red classification for context evals (issue #115).
//!
//! The grader inspects process results, workspaces, and store state. It never reads a
//! model-authored claim as evidence of success, so a run that merely *says* it finished
//! cannot pass. Expected red (a clean, predicted failure of the acceptance target) is
//! distinguished from a harness error (infrastructure failure) and from an unexpected
//! outcome (a red scenario that passed, or a failure of the wrong class).

use crate::context_eval::manifest::{ExpectedStatus, Scenario};
use std::path::PathBuf;

/// Independent evidence drawn from process results, the workspace, and the session store.
/// Attribute lists are spelled out so no derive macro ever needs a compressed form.
#[derive(Clone, Default)]
pub struct Evidence {
    /// Spawn failure, broken stdout contract, timeout, or signal death.
    pub harness_error: bool,
    pub turns_total: usize,
    pub turns_ok: usize,
    pub last_ok_summary: String,
    pub last_ok_stdout: Vec<u8>,
    pub session_dir: Option<PathBuf>,
    pub context_limit_hit: bool,
    pub provider_requests: usize,
    pub tool_calls_scripted: usize,
    /// The scripted provider actually emitted the final marker response.
    pub final_response_issued: bool,
    /// Terminal outcome declared by the context runtime, when one exists.
    pub terminal_outcome: Option<String>,
    /// Failure is a clean resource-bound hit (the wall) with the task otherwise sound.
    pub resource_limit_hit: bool,
    /// Fault-triggered death with persisted state restored to a consistent shape.
    pub recovery_after_fault: bool,
    /// Planted fixture markers escaped into outputs, stores, or artifacts.
    pub leaks: Vec<(String, String)>,
    /// Executed fault triggers with their loopback-observed trigger points.
    pub faults_executed: Vec<String>,
    /// Serialized request sizes observed by the loopback, one per request.
    pub request_bytes: Vec<usize>,
    /// Largest serialized provider request the loopback observed.
    pub max_request_bytes: usize,
    /// Serialized size of the request that preceded a refusal (0 when none).
    pub last_request_bytes: usize,
    /// How many observed requests asked for a streamed response.
    pub streamed_requests: usize,
    /// Distinct tool names the runner offered, in first-seen order.
    pub tool_names: Vec<String>,
    /// SHA-256 over the concatenated observed request bodies (ordering-sensitive).
    pub request_bodies_digest: String,
}

/// Graded result for one scenario.
#[derive(Clone, Default)]
pub struct Graded {
    pub passed: bool,
    pub reason_class: String,
    pub failures: Vec<String>,
    pub harness_error: bool,
}

/// One evidence dimension, graded separately from task correctness (R-016, GAP-M15).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dimension {
    /// Did the runner produce a correct answer / honest completion?
    Task,
    /// Did protocol and state invariants hold (no forged marker, clean store)?
    Protocol,
    /// Resource usage: wall behavior and bounded growth.
    Resource,
    /// Latency: the declared invocation budget was honored end to end.
    ///
    /// Phase 0 records no wall-clock threshold: the loopback is in-process, so a timing
    /// oracle would only measure harness noise. The typed signal is the invocation budget
    /// a scenario declares, because a run that stops before its declared prompts leaves
    /// followup work unanswered, which is a service-level (latency) defect distinct from
    /// the task answer and from resource use.
    Latency,
    /// Recovery: restart/crash faults land and persisted state stays consistent.
    Recovery,
    /// Wall realism: the wall fires on the real materialized request, and any
    /// refusal is preceded by admission pressure the loopback actually observed.
    WallRealism,
}

impl Dimension {
    /// Stable field name for reports.
    pub fn field(self) -> &'static str {
        match self {
            Dimension::Task => "task",
            Dimension::Protocol => "protocol",
            Dimension::Resource => "resource",
            Dimension::Latency => "latency",
            Dimension::Recovery => "recovery",
            Dimension::WallRealism => "wall_realism",
        }
    }
}

/// All dimensions with their pass/fail state and the failures charged to each.
pub fn dimension_results(scen: &Scenario, ev: &Evidence) -> Vec<(Dimension, bool, Vec<String>)> {
    let mut dims: Vec<(Dimension, bool, Vec<String>)> = Vec::new();

    let task = task_failures(scen, ev);
    dims.push((Dimension::Task, task.is_empty(), task));

    let mut protocol = protocol_failures(scen, ev);
    if ev.harness_error {
        protocol.push("harness infrastructure failure".to_string());
    }
    for (marker, where_) in ev.leaks.iter().take(8) {
        protocol.push(format!("planted marker {marker} leaked into {where_}"));
    }
    dims.push((Dimension::Protocol, protocol.is_empty(), protocol));

    let mut resource = Vec::new();
    if ev.resource_limit_hit && ev.turns_ok == 0 {
        resource.push("resource bound hit without any successful turn".to_string());
    }
    dims.push((Dimension::Resource, resource.is_empty(), resource));

    let latency = latency_failures(scen, ev);
    dims.push((Dimension::Latency, latency.is_empty(), latency));

    let wants_fault = !scen.faults.injected.is_empty();
    let mut recovery = Vec::new();
    if wants_fault && ev.faults_executed.is_empty() {
        recovery.push(format!(
            "scenario declares faults {:?} but none executed",
            scen.faults.injected
        ));
    }
    if wants_fault && !ev.recovery_after_fault {
        recovery.push(
            "persisted state was not restored to a consistent shape after the fault".to_string(),
        );
    }
    dims.push((Dimension::Recovery, recovery.is_empty(), recovery));

    let mut wall = Vec::new();
    if ev.context_limit_hit && ev.provider_requests == 0 {
        wall.push("refusal recorded without any provider request observed".to_string());
    }
    dims.push((Dimension::WallRealism, wall.is_empty(), wall));

    dims
}

/// Latency/service-level failures: a scenario's declared invocation budget.
///
/// `prompts()` is the scenario's own declared budget (opening prompt plus followups), and
/// `turns_total` is what the drive actually ran. The comparison is against the scenario's
/// own declaration, never a clock, so this dimension stays deterministic and offline.
/// A fault-scenario death is charged to recovery, not here, so a killed run is not
/// double-billed for the turns the fault itself removed.
fn latency_failures(scen: &Scenario, ev: &Evidence) -> Vec<String> {
    if ev.faults_executed.is_empty() && ev.turns_total < scen.prompts().len() {
        return vec![format!(
            "invocation budget not honored: {} of {} declared prompts were driven",
            ev.turns_total,
            scen.prompts().len()
        )];
    }
    Vec::new()
}

fn evidence_failures(scen: &Scenario, ev: &Evidence) -> Vec<String> {
    let mut out = Vec::new();
    for token in &scen.assertions.required_answer_tokens {
        if !ev.last_ok_summary.contains(token.as_str())
            && !window(ev.last_ok_stdout.as_slice()).contains(token.as_str())
        {
            out.push(format!("required answer token absent: {token}"));
        }
    }
    for rel in &scen.assertions.required_context_artifacts {
        match &ev.session_dir {
            None => out.push(format!("no persisted session to hold {rel}")),
            Some(dir) => {
                if !dir.join(rel).exists() {
                    out.push(format!("required context artifact absent: {rel}"));
                }
            }
        }
    }
    if !scen.assertions.required_outcomes.is_empty()
        && !scen
            .assertions
            .required_outcomes
            .iter()
            .any(|outcome| ev.terminal_outcome.as_deref() == Some(outcome.as_str()))
    {
        out.push(format!(
            "required terminal outcome absent: one of [{}]",
            scen.assertions.required_outcomes.join(", ")
        ));
    }
    out
}

fn task_failures(scen: &Scenario, ev: &Evidence) -> Vec<String> {
    let mut out = Vec::new();
    if ev.turns_ok == 0 {
        out.push("no turn completed successfully".to_string());
    }
    if let Some(marker) = &scen.assertions.required_final_marker {
        let present = ev.last_ok_summary.contains(marker.as_str())
            || window(ev.last_ok_stdout.as_slice()).contains(marker.as_str());
        if !present {
            out.push(format!("required final marker absent: {marker}"));
        }
    }
    out
}

/// A marker in the output that the provider never issued is a protocol break, not a
/// task failure and never harness infrastructure: the run fabricated success evidence.
fn protocol_failures(scen: &Scenario, ev: &Evidence) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(marker) = &scen.assertions.required_final_marker {
        let present = ev.last_ok_summary.contains(marker.as_str())
            || window(ev.last_ok_stdout.as_slice()).contains(marker.as_str());
        if present && !ev.final_response_issued {
            out.push(format!(
                "final marker {marker} appeared in output without the provider ever issuing it"
            ));
        }
    }
    out
}

/// Lossy bounded view of raw stdout used only for exact planted-token searches.
fn window(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

/// Grade one scenario against independent evidence.
pub fn grade(scen: &Scenario, ev: &Evidence) -> Graded {
    let mut failures = Vec::new();
    let dims = dimension_results(scen, ev);
    for (dim, _ok, dim_failures) in &dims {
        for failure in dim_failures {
            failures.push(format!("[{}] {failure}", dim.field()));
        }
    }
    let evidence = evidence_failures(scen, ev);
    failures.extend(evidence.iter().cloned());
    let passed = failures.is_empty();
    let reason_class = classify(scen, ev, &failures);
    Graded {
        passed,
        reason_class,
        failures,
        harness_error: ev.harness_error,
    }
}

fn classify(scen: &Scenario, ev: &Evidence, failures: &[String]) -> String {
    if ev.harness_error {
        return "harness-error".to_string();
    }
    if !ev.leaks.is_empty() {
        return "leakage".to_string();
    }
    if ev.context_limit_hit {
        return "context-limit".to_string();
    }
    if ev.resource_limit_hit {
        return "resource-limit".to_string();
    }
    if !ev.faults_executed.is_empty() && !ev.recovery_after_fault {
        return "recovery-failure".to_string();
    }
    let evidence_missing = failures
        .iter()
        .any(|f| f.contains("absent") || f.contains("no persisted session"));
    if evidence_missing {
        return "missing-evidence".to_string();
    }
    let _ = scen;
    "task-failure".to_string()
}

/// Verdict for one scenario against its expected status.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The scenario passed and was expected to pass.
    Pass,
    /// The scenario failed exactly as predicted for this phase.
    ExpectedRed,
    /// A red scenario passed: would be a false success if unflagged.
    UnexpectedGreen,
    /// The scenario failed, but not for the reason this phase predicts.
    UnexpectedRedReason,
    /// Infrastructure failed; no statement about the acceptance target is possible.
    HarnessError,
}

impl Verdict {
    /// Whether this verdict satisfies the phase's expected-status contract.
    pub fn accepted(&self) -> bool {
        matches!(self, Verdict::Pass | Verdict::ExpectedRed)
    }

    /// Stable machine-readable name for reports.
    pub fn name(&self) -> &'static str {
        match self {
            Verdict::Pass => "pass",
            Verdict::ExpectedRed => "expected-red",
            Verdict::UnexpectedGreen => "unexpected-green",
            Verdict::UnexpectedRedReason => "unexpected-red-reason",
            Verdict::HarnessError => "harness-error",
        }
    }
}

/// Classify a graded scenario against its declared expected status.
///
/// Reason matching is strict: a red scenario is predicted *for its declared reason
/// class*, and only `accept_any_reason = true` broadens that to any clean failure.
/// There is no implicit leniency for `missing-evidence`.
pub fn verdict(scen: &Scenario, graded: &Graded) -> Verdict {
    if graded.harness_error {
        return Verdict::HarnessError;
    }
    if graded.passed {
        return match scen.expected_status {
            ExpectedStatus::Green => Verdict::Pass,
            ExpectedStatus::Red => Verdict::UnexpectedGreen,
        };
    }
    let reason_ok = scen.accept_any_reason || graded.reason_class == scen.expected_reason_class;
    match scen.expected_status {
        ExpectedStatus::Red if reason_ok => Verdict::ExpectedRed,
        _ => Verdict::UnexpectedRedReason,
    }
}
