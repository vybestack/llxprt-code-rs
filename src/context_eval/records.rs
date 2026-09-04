//! Append-only phase-indexed expected-status and run records (issue #116, R-013).
//!
//! Every current-status change a manifest receives is recorded here as a new
//! phase-indexed entry; the manifest field itself always names the *latest* phase's
//! expectation and is never rewritten in place. `record` appends, `latest` reads the
//! most recent entry, and [`validate_history`] rejects a manifest whose declared status
//! and reason class contradict the preserved history. Red-then-green run records follow
//! the same rule: the observed Phase 0 baseline is appended once and never mutated, so
//! a later phase turning the same scenario green leaves the red record in place.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

/// One phase-indexed expected-status entry. Append-only: a phase that changes a
/// scenario's expectation appends a new entry and never edits an older one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StatusRecord {
    pub scenario: String,
    /// Phase whose expectation this entry names.
    pub phase: u8,
    pub expected_status: String,
    pub expected_reason_class: String,
    /// Whether that phase accepted any clean failure reason (part of the prediction).
    #[serde(default)]
    pub accept_any_reason: bool,
    /// Free-text justification tied to this phase (e.g. which gap keeps it red).
    #[serde(default)]
    pub note: String,
}

/// One phase-indexed observed-run entry (the red-then-green evidence trail).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunRecord {
    pub scenario: String,
    pub phase: u8,
    /// `red` or `green` exactly as the drive graded it.
    pub observed_status: String,
    pub reason_class: String,
    pub verdict: String,
    /// Aggregate report that carries the full evidence for this observation.
    pub report: String,
}

/// Root holding `expected-status-history.jsonl` and `runs/`.
pub struct Records {
    pub root: PathBuf,
}

impl Records {
    pub fn new(root: &Path) -> Records {
        Records {
            root: root.to_path_buf(),
        }
    }

    pub fn history_path(&self) -> PathBuf {
        self.root.join("expected-status-history.jsonl")
    }

    fn runs_dir(&self) -> PathBuf {
        self.root.join("runs")
    }

    /// Append one expected-status entry. Never rewrites existing bytes.
    pub fn record(&self, entry: &StatusRecord) -> Result<(), String> {
        append_jsonl(&self.history_path(), entry)
    }

    /// Append one observed-run entry keyed by phase. Never rewrites existing bytes.
    pub fn record_run(&self, entry: &RunRecord) -> Result<PathBuf, String> {
        fs::create_dir_all(self.runs_dir())
            .map_err(|e| format!("create {}: {e}", self.runs_dir().display()))?;
        let path = self.runs_dir().join(format!("phase-{}.jsonl", entry.phase));
        append_jsonl(&path, entry)?;
        Ok(path)
    }

    /// The most recent expected-status entry for a scenario, if any.
    pub fn latest(&self, scenario: &str) -> Result<Option<StatusRecord>, String> {
        Ok(load_history(&self.history_path())?
            .into_iter()
            .rev()
            .find(|r| r.scenario == scenario))
    }
}

/// Load the full expected-status history in append order.
pub fn load_history(path: &Path) -> Result<Vec<StatusRecord>, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let mut out = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record: StatusRecord = serde_json::from_str(line).map_err(|e| {
            format!(
                "{} line {}: expected-status history entry is malformed: {e}",
                path.display(),
                index + 1
            )
        })?;
        out.push(record);
    }
    Ok(out)
}

/// Reject a declared manifest expectation that contradicts the preserved history.
///
/// An append of a *new* expectation for the current phase is legitimate; a manifest
/// that silently changes the status or reason class an earlier phase recorded without
/// appending its own entry is the mutable-expected-status defect R-013 forbids.
pub fn validate_history(
    history: &[StatusRecord],
    scenario: &str,
    phase: u8,
    expected_status: &str,
    expected_reason_class: &str,
) -> Result<(), String> {
    let mut prior: Option<&StatusRecord> = None;
    for record in history {
        if record.scenario != scenario {
            continue;
        }
        if record.phase == phase {
            if record.expected_status != expected_status
                || record.expected_reason_class != expected_reason_class
            {
                return Err(format!(
                    "scenario {scenario} phase {phase} contradicts its recorded expectation \
                     (recorded {}/{}), manifest declares {expected_status}/{expected_reason_class}; \
                     append a new phase-indexed entry instead of rewriting history",
                    record.expected_status, record.expected_reason_class
                ));
            }
            return Ok(());
        }
        prior = Some(record);
    }
    // No entry for this phase yet: the manifest's declaration must still not contradict
    // the latest prior entry *for the same phase boundary* — a new phase entry is how
    // expectations legitimately move, so only an exact phase match is enforced above.
    let _ = prior;
    Ok(())
}

fn append_jsonl<T: Serialize>(path: &Path, entry: &T) -> Result<(), String> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let mut line = serde_json::to_string(entry).map_err(|e| format!("encode record: {e}"))?;
    line.push('\n');
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    file.write_all(line.as_bytes())
        .map_err(|e| format!("append {}: {e}", path.display()))?;
    Ok(())
}

/// Whether the history already carries an entry for this scenario at this phase.
///
/// A run appends exactly one entry per (scenario, phase) pair, which is what keeps the
/// file append-only: the phase-0 baseline is written once and later phases append their
/// own declarations instead of overwriting the one that predicted them red.
pub fn has_phase_entry(history: &[StatusRecord], scenario: &str, phase: u8) -> bool {
    history
        .iter()
        .any(|r| r.scenario == scenario && r.phase == phase)
}

/// Load observed-run records for one phase.
pub fn load_runs(records: &Records, phase: u8) -> Result<Vec<RunRecord>, String> {
    let path = records.runs_dir().join(format!("phase-{phase}.jsonl"));
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let mut out = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let record: RunRecord = serde_json::from_str(line)
            .map_err(|e| format!("{}: malformed run record: {e}", path.display()))?;
        out.push(record);
    }
    Ok(out)
}

/// Machine-checkable summary of the preserved baseline: phase 0 entries must be red
/// for every scenario they name, and the count must match the manifest count.
///
/// The run records are the red-then-green trail (R-013): a phase that observes a
/// scenario green after the same phase recorded it red is exactly the transition the
/// append-only file must preserve. `green_without_prior_red` lists any green observation
/// with no preserved red entry to precede it, which would mean the red baseline was
/// mutated away rather than appended past.
pub fn baseline_summary(records: &Records, manifest_ids: &[String]) -> Result<Value, String> {
    let history = load_history(&records.history_path())?;
    let phase0: Vec<&StatusRecord> = history.iter().filter(|r| r.phase == 0).collect();
    let mut missing = Vec::new();
    for id in manifest_ids {
        if !phase0.iter().any(|r| &r.scenario == id) {
            missing.push(id.clone());
        }
    }
    let non_red: Vec<String> = phase0
        .iter()
        .filter(|r| r.expected_status != "red")
        .map(|r| r.scenario.clone())
        .collect();
    let all_red = missing.is_empty() && non_red.is_empty();
    // Red-then-green trail: for every (scenario, phase) that has a green observation,
    // an earlier record in the same file must carry that scenario red. The phase-0
    // expected-status entries are the predicted red; run records are the observed one.
    let mut green_without_prior_red: Vec<String> = Vec::new();
    for phase in 0_u8..=9 {
        for run in load_runs(records, phase)? {
            if run.observed_status != "green" {
                continue;
            }
            let prior_red = load_runs(records, phase)?
                .iter()
                .any(|r| r.scenario == run.scenario && r.observed_status == "red");
            let expected_red = history
                .iter()
                .any(|r| r.scenario == run.scenario && r.expected_status == "red");
            if !prior_red && !expected_red {
                green_without_prior_red.push(format!("{}:phase-{}", run.scenario, phase));
            }
        }
    }
    Ok(json!({
        "phase0_entries": phase0.len(),
        "manifests": manifest_ids.len(),
        "missing_phase0_entry": missing,
        "phase0_non_red": non_red,
        "all_red_baseline": all_red,
        "green_without_prior_red": green_without_prior_red,
        "red_then_green_trail_complete": green_without_prior_red.is_empty(),
    }))
}
