//! Independent grader and expected-red classification for context evals (#37).
//!
//! The grader inspects process results, workspaces, and store state. It never reads a
//! model-authored claim as evidence of success, so a run that merely *says* it finished
//! cannot pass. Expected red (a clean, predicted failure of the acceptance target) is
//! distinguished from a harness error (infrastructure failure) and from an unexpected
//! outcome (a red scenario that passed, or a failure of the wrong class).

use crate::context_eval::manifest::{ExpectedStatus, Scenario};
use std::path::PathBuf;

/// Independent evidence drawn from process results, the workspace, and the session store.
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
}

/// Graded result for one scenario.
#[derive(Debug)]
pub struct Graded {
    pub passed: bool,
    pub reason_class: String,
    pub failures: Vec<String>,
    pub harness_error: bool,
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
    for outcome in &scen.assertions.required_outcomes {
        if ev.terminal_outcome.as_deref() != Some(outcome.as_str()) {
            out.push(format!("required terminal outcome absent: {outcome}"));
        }
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
        if present && !ev.final_response_issued {
            // A model-authored claim is not evidence: the provider never issued the
            // marker, so the run cannot have produced it honestly.
            out.push(format!(
                "final marker {marker} appeared in output without the provider ever issuing it"
            ));
        } else if !present {
            out.push(format!("required final marker absent: {marker}"));
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
    if ev.harness_error {
        failures.push("harness infrastructure failure".to_string());
    }
    let evidence = evidence_failures(scen, ev);
    let task = task_failures(scen, ev);
    failures.extend(evidence.iter().cloned());
    failures.extend(task.iter().cloned());
    let reason_class = if ev.harness_error {
        "harness-error"
    } else if ev.context_limit_hit {
        "context-limit"
    } else if !evidence.is_empty() {
        "missing-evidence"
    } else {
        "task-failure"
    };
    Graded {
        passed: failures.is_empty(),
        reason_class: reason_class.to_string(),
        failures,
        harness_error: ev.harness_error,
    }
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
    let reason_ok = graded.reason_class == scen.expected_reason_class
        || graded.reason_class == "missing-evidence"
        || scen.accept_any_reason;
    match scen.expected_status {
        ExpectedStatus::Red if reason_ok => Verdict::ExpectedRed,
        _ => Verdict::UnexpectedRedReason,
    }
}
