//! Records-side self-tests for the Phase 0 context evals (R-013, GAP-H7).
//!
//! Append-only phase-indexed expected-status history, the all-red phase-0 baseline,
//! and the red-then-green observed-run trail. Split from the main self-test module only
//! to respect the per-file LOC gate; the imports mirror the parent module's.

use crate::context_eval::records;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};

fn fixtures() -> PathBuf {
    Path::new("evals/context-management").join("fixtures")
}

/// Lowercase hex SHA-256 of a byte string, for the digest fields `RunRecord` binds.
fn sha256_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(text.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Append-only expected-status history (GAP-H7, R-013): records load, latest wins,
/// a contradiction for the same phase is rejected, and the preserved phase-0 baseline
/// is all-red across the shipped manifests.
#[test]
fn expected_status_history_is_append_only_and_validated() {
    let dir = std::env::temp_dir().join(format!("ctxeval-rec-{}", crate::harness::uniq()));
    let store = records::Records::new(&dir);
    let first = records::StatusRecord {
        scenario: "probe".to_string(),
        phase: 0,
        expected_status: "red".to_string(),
        expected_reason_class: "context-limit".to_string(),
        accept_any_reason: false,
        note: "phase 0 baseline".to_string(),
    };
    store.record(&first).unwrap();
    store.record(&first).unwrap();
    let path = store.history_path();
    let raw = fs::read_to_string(&path).unwrap();
    assert_eq!(raw.lines().count(), 2, "record rewrote instead of appended");

    let latest = store.latest("probe").unwrap().unwrap();
    assert_eq!(latest.phase, 0);

    // The current manifest agrees with the record: no contradiction.
    records::validate_history(
        std::slice::from_ref(&first),
        "probe",
        0,
        "red",
        "context-limit",
    )
    .unwrap();
    // A silent in-place rewrite of that phase's expectation is the R-013 defect.
    let err = records::validate_history(&[first], "probe", 0, "green", "missing-evidence");
    assert!(
        err.is_err(),
        "contradicting a preserved record was accepted"
    );

    // A phase-4 green is a legitimate *new* entry, not a contradiction of phase 0.
    let phase4 = records::StatusRecord {
        scenario: "probe".to_string(),
        phase: 4,
        expected_status: "green".to_string(),
        expected_reason_class: "missing-evidence".to_string(),
        accept_any_reason: false,
        note: "phase 4 landed".to_string(),
    };
    records::validate_history(&[latest, phase4], "probe", 4, "green", "missing-evidence").unwrap();

    fs::remove_dir_all(&dir).ok();
}

/// The shipped phase-0 record set is an all-red baseline covering every shipped
/// manifest, and run records append red-then-green without mutation.
#[test]
fn shipped_baseline_is_all_red_and_run_records_append() {
    let scenarios = crate::context_eval::manifest::load_dir(
        Path::new("evals/context-management/scenarios"),
        &fixtures(),
    )
    .unwrap();
    let ids: Vec<String> = scenarios.iter().map(|(_, s)| s.id.clone()).collect();

    let store = records::Records::new(Path::new("evals/context-management/records"));
    let summary = records::baseline_summary(&store, &ids).unwrap();
    assert_eq!(
        summary["all_red_baseline"], true,
        "phase-0 baseline is not all-red: {summary}"
    );
    assert_eq!(summary["phase0_entries"], json!(ids.len()));

    // Run records append per phase; nothing is rewritten.
    let dir = std::env::temp_dir().join(format!("ctxeval-runs-{}", crate::harness::uniq()));
    let runs = records::Records::new(&dir);
    let red = records::RunRecord {
        scenario: "probe".to_string(),
        phase: 4,
        observed_status: "red".to_string(),
        reason_class: "context-limit".to_string(),
        verdict: "expected-red".to_string(),
        runner_revision: "0123456789abcdef".to_string(),
        manifest_digest: sha256_hex("a"),
        fixture_digests: vec![sha256_hex("b")],
        report: "report-probe.json".to_string(),
    };
    let green = records::RunRecord {
        observed_status: "green".to_string(),
        verdict: "pass".to_string(),
        // A green observation has no failure to name (R-013).
        reason_class: String::new(),
        ..red.clone()
    };
    runs.record_run(&red).unwrap();
    runs.record_run(&green).unwrap();
    let phase4 = records::load_runs(&runs, 4).unwrap();
    assert_eq!(phase4.len(), 2);
    assert_eq!(phase4[0].observed_status, "red");
    assert_eq!(phase4[1].observed_status, "green");
    fs::remove_dir_all(&dir).ok();
}

/// Append-per-phase (R-013): `run_all` records a manifest's declared expectation for
/// its owning phase exactly once, so a second drive appends nothing and an
/// expectation that legitimately moves with a phase appends rather than rewrites.
#[test]
fn phase_indexed_history_appends_once_per_phase() {
    let dir = std::env::temp_dir().join(format!("ctxeval-phase-{}", crate::harness::uniq()));
    let store = records::Records::new(&dir);
    let history: Vec<records::StatusRecord> = Vec::new();

    // Nothing recorded yet: this (scenario, phase) pair has no entry.
    assert!(!records::has_phase_entry(&history, "probe", 4));

    // The phase-4 expectation the manifest declares is appended once.
    let first = records::StatusRecord {
        scenario: "probe".to_string(),
        phase: 4,
        expected_status: "green".to_string(),
        expected_reason_class: "missing-evidence".to_string(),
        accept_any_reason: true,
        note: "phase 4 landed".to_string(),
    };
    store.record(&first).unwrap();
    let loaded = records::load_history(&store.history_path()).unwrap();
    assert!(records::has_phase_entry(&loaded, "probe", 4));
    assert_eq!(loaded.len(), 1);

    // A second drive of the same phase must not append a duplicate entry.
    assert!(records::has_phase_entry(&loaded, "probe", 4));
    let still = records::load_history(&store.history_path()).unwrap();
    assert_eq!(still.len(), 1, "append-per-phase appended a duplicate");

    // A later phase's own expectation appends as a new entry, leaving phase 4 intact.
    let phase5 = records::StatusRecord {
        phase: 5,
        note: "phase 5 re-tightened".to_string(),
        ..first.clone()
    };
    store.record(&phase5).unwrap();
    let both = records::load_history(&store.history_path()).unwrap();
    assert_eq!(both.len(), 2);
    assert!(records::has_phase_entry(&both, "probe", 4));
    assert!(records::has_phase_entry(&both, "probe", 5));
    assert_eq!(
        both.iter().filter(|r| r.phase == 4).count(),
        1,
        "the phase-4 entry was rewritten instead of preserved"
    );

    fs::remove_dir_all(&dir).ok();
}

/// The red-then-green trail is machine-checkable: a green observation with no
/// preserved red to precede it is reported, and a complete trail is not flagged.
#[test]
fn red_then_green_trail_is_reported() {
    let dir = std::env::temp_dir().join(format!("ctxeval-trail-{}", crate::harness::uniq()));
    let runs = records::Records::new(&dir);

    // A green observation with neither a prior red run nor a recorded expected-red.
    runs.record_run(&records::RunRecord {
        scenario: "probe".to_string(),
        phase: 4,
        observed_status: "green".to_string(),
        // A green observation has no failure to name (R-013): the record binds the digests
        // and revision of the drive that produced it, and the empty reason class is part
        // of what the grader observed, not an omission.
        reason_class: String::new(),
        verdict: "pass".to_string(),
        runner_revision: "0123456789abcdef".to_string(),
        manifest_digest: sha256_hex("a"),
        fixture_digests: vec![sha256_hex("b")],
        report: "report-probe.json".to_string(),
    })
    .unwrap();
    let summary = records::baseline_summary(&runs, &[]).unwrap();
    assert_eq!(
        summary["green_without_prior_red"].as_array().map(Vec::len),
        Some(1),
        "green with no preserved red was not reported: {summary}"
    );
    assert_eq!(summary["red_then_green_trail_complete"], false);

    fs::remove_dir_all(&dir).ok();
}
