//! Append-only phase-indexed expected-status and run records (issue 37, R-013).
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
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// One phase-indexed expected-status entry. Append-only: a phase that changes a
/// scenario's expectation appends a new entry and never edits an older one.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
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
///
/// The record is the binding the issue requires: it names the source revision that ran,
/// the digests of the manifests and fixtures that were driven, and the report file that
/// carries the full evidence. `report` is always a path *relative to the repository
/// root* naming the aggregate report file, never an absolute developer path and never a
/// directory, so the trail stays checkable on any checkout.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunRecord {
    pub scenario: String,
    pub phase: u8,
    /// `red` or `green` exactly as the drive graded it.
    pub observed_status: String,
    /// Failure reason class. Empty for a green observation: a pass has no failure to name.
    #[serde(default)]
    pub reason_class: String,
    pub verdict: String,
    /// Source revision the acceptance target was built from.
    #[serde(default)]
    pub runner_revision: String,
    /// SHA-256 of the scenario manifest's bytes.
    #[serde(default)]
    pub manifest_digest: String,
    /// SHA-256 of every expanded fixture round, in drive order.
    #[serde(default)]
    pub fixture_digests: Vec<String>,
    /// Aggregate report file, relative to the repository root (never absolute).
    #[serde(default)]
    pub report: String,
}

impl RunRecord {
    /// Whether this record is internally well formed for the phase trail it claims.
    ///
    /// `report` must be a relative path to a `.json` file (never absolute, never a
    /// directory), the manifest digest must be 64 lowercase hex characters, and a green
    /// observation must not carry a failure reason class.
    pub fn well_formed(&self) -> Result<(), String> {
        if self.observed_status != "red" && self.observed_status != "green" {
            return Err(format!(
                "scenario {} phase {} has observed_status {} (red/green expected)",
                self.scenario, self.phase, self.observed_status
            ));
        }
        let rel = Path::new(&self.report);
        if self.report.is_empty() {
            return Err(format!(
                "scenario {} phase {} records an empty report path",
                self.scenario, self.phase
            ));
        }
        if rel.is_absolute() {
            return Err(format!(
                "scenario {} phase {} records the absolute report path {}",
                self.scenario, self.phase, self.report
            ));
        }
        if rel.extension().and_then(|e| e.to_str()) != Some("json") {
            return Err(format!(
                "scenario {} phase {} records report {} which is not a report file",
                self.scenario, self.phase, self.report
            ));
        }
        if rel
            .components()
            .any(|c| c == std::path::Component::ParentDir)
        {
            return Err(format!(
                "scenario {} phase {} records report {} with a parent-directory component",
                self.scenario, self.phase, self.report
            ));
        }
        if !is_sha256_hex(&self.manifest_digest) {
            return Err(format!(
                "scenario {} phase {} records manifest digest {} which is not a SHA-256",
                self.scenario, self.phase, self.manifest_digest
            ));
        }
        for digest in &self.fixture_digests {
            if !is_sha256_hex(digest) {
                return Err(format!(
                    "scenario {} phase {} records fixture digest {digest} which is not a \
                     SHA-256",
                    self.scenario, self.phase
                ));
            }
        }
        if self.observed_status == "green" && !self.reason_class.is_empty() {
            return Err(format!(
                "scenario {} phase {} records green with the failure reason class {}",
                self.scenario, self.phase, self.reason_class
            ));
        }
        Ok(())
    }
}

fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())
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
    ///
    /// The entry is shape-checked on the way in, so a malformed trail cannot be created
    /// by a harness bug and then read back as if it were evidence.
    pub fn record_run(&self, entry: &RunRecord) -> Result<PathBuf, String> {
        entry
            .well_formed()
            .map_err(|e| format!("refusing to record a malformed run entry: {e}"))?;
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
/// Two checks, both against the append-only file:
///
/// * An entry for the *same phase* must match the manifest's declaration exactly; a
///   mismatch is the silent in-place edit R-013 forbids.
/// * When the manifest declares **green** for phase N, the most recent entry before
///   N (the `prior` this function used to discard) must have recorded **red** for the
///   same scenario. A phase may only move a scenario to green past a preserved red
///   expectation, never around one.
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
                     (recorded {}/{}), manifest declares {expected_status}/\
                     {expected_reason_class}; append a new phase-indexed entry instead of \
                     rewriting history",
                    record.expected_status, record.expected_reason_class
                ));
            }
            return Ok(());
        }
        prior = Some(record);
    }
    // No entry for this phase yet. A new phase entry is how expectations legitimately
    // move, so an exact phase match is the only in-place-edit check — but a green
    // declaration must still clear the prior-red bar the #116 semantics set.
    if let Some(prior) = prior {
        if expected_status == "green" && prior.expected_status != "red" {
            return Err(format!(
                "scenario {scenario} phase {phase} declares green but the latest preserved \
                 entry (phase {}) still records {}; green may only follow a recorded red",
                prior.phase, prior.expected_status
            ));
        }
    } else if expected_status == "green" {
        return Err(format!(
            "scenario {scenario} phase {phase} declares green with no prior recorded \
             expectation; green may only follow a recorded red"
        ));
    }
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
        record
            .well_formed()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        out.push(record);
    }
    Ok(out)
}

/// Highest phase a run record exists for (so a caller can sweep the whole trail).
pub fn max_recorded_phase(records: &Records) -> u8 {
    let mut max = 0_u8;
    if let Ok(entries) = fs::read_dir(records.root.join("runs")) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(stem) = name.strip_suffix(".jsonl") else {
                continue;
            };
            let Some(stem) = stem.strip_prefix("phase-") else {
                continue;
            };
            if let Ok(phase) = stem.parse::<u8>() {
                max = max.max(phase);
            }
        }
    }
    max
}

/// Observed red run records that exist for `scenario` at `phase`.
///
/// This is the #116.1 bar and the only bar: a green for phase N is expressible only when
/// an *observed* red `RunRecord` exists for the same phase, because an expected-status
/// history entry is a prediction, never an observation.
pub fn prior_observed_red(records: &Records, scenario: &str, phase: u8) -> Vec<RunRecord> {
    load_runs(records, phase)
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.scenario == scenario && r.observed_status == "red")
        .collect()
}

/// Machine-checkable summary of the preserved baseline: phase 0 entries must be red
/// for every scenario they name, and the count must match the manifest count.
///
/// The run records are the red-then-green trail (R-013, #116.1): a green observation is
/// licensed only by an observed red `RunRecord` at the same phase in the same append-only
/// file. An expected-status history entry never licenses green — a prediction is not a
/// red that was observed — so `green_without_prior_red` lists every green observation with
/// no observed red to precede it.
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
    // Red-then-green trail (#116.1): for every (scenario, phase) carrying a green
    // observation, the same phase file must also carry that scenario's observed red.
    let mut green_without_prior_red: Vec<String> = Vec::new();
    let mut green_scenarios: BTreeMap<String, u8> = BTreeMap::new();
    for phase in 0_u8..=max_recorded_phase(records) {
        let runs = load_runs(records, phase)?;
        for run in runs.iter().filter(|r| r.observed_status == "green") {
            let has_observed_red = runs
                .iter()
                .any(|r| r.scenario == run.scenario && r.observed_status == "red");
            if !has_observed_red {
                green_without_prior_red.push(format!("{}:phase-{phase}", run.scenario));
            }
            green_scenarios.insert(run.scenario.clone(), phase);
        }
    }
    green_without_prior_red.sort();
    green_without_prior_red.dedup();
    Ok(json!({
        "phase0_entries": phase0.len(),
        "manifests": manifest_ids.len(),
        "missing_phase0_entry": missing,
        "phase0_non_red": non_red,
        "all_red_baseline": all_red,
        "green_without_prior_red": green_without_prior_red,
        "green_observations": green_scenarios.len(),
        "red_then_green_trail_complete": green_without_prior_red.is_empty(),
    }))
}
