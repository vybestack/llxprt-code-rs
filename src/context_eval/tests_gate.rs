//! Self-tests for the #143 green-gate lifecycle: green is licensed by an *observed* red
//! `RunRecord` from the same scenario at the owning phase or any earlier phase, never by
//! a prediction and never by a red driven from the citing manifest's own bytes.
//!
//! The drives here are fully hermetic: the acceptance target is a `/bin/sh` stand-in
//! that emits one valid success envelope and exits 0, and the scenario declares its
//! terminal outcome (`required_outcomes`) instead of a final marker, so a pass needs no
//! provider traffic at all. One sibling test closes the verifier-noted negative-test gap
//! in report validation (empty nested objects, unknown verdicts, unclean leak scans).

use crate::agent::prompt_digest;
use crate::context_eval::manifest;
use crate::context_eval::records::{self, RunRecord, StatusRecord};
use crate::context_eval::report;
use crate::context_eval::{run_all, Options, RunnerKind};
use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Fresh tempdir per test, unique so tests can run in parallel.
fn tempdir(prefix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ctxeval-{prefix}-{}", crate::harness::uniq()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Fresh out root *under the working directory* (repo-local like every real run), so the
/// aggregate report path recorded in run records is repository-relative: an absolute
/// path is exactly what `RunRecord::well_formed` refuses.
fn out_root(prefix: &str) -> PathBuf {
    let dir = Path::new("tmp").join(format!("ctxeval-{prefix}-{}", crate::harness::uniq()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A minimal gate-probe manifest: no wall pressure, no marker, one declared terminal
/// outcome. `phase`/`expected`/`reason` select the declaration under test.
fn gate_manifest(phase: u8, expected: &str, reason: &str) -> String {
    format!(
        r#"
schema_version = 1
id = "gate-probe"
owner_phase = {phase}
arm = "feature"
expected_status = "{expected}"
expected_reason_class = "{reason}"
accept_any_reason = false

[profile]
name = "ctxeval-loopback"
provider = "openai"
model = "ctxeval-fixture"
context_limit_tokens = 20000
max_output_tokens = 2048

[stimulus]
prompt = "CTXEVAL-GATE read the workspace and wrap up"

[assertions]
required_outcomes = ["wrap_up"]

[runtime]
name = "profile-limit"
context_limit = 20000
"#
    )
}

const GATE_PROMPT: &str = "CTXEVAL-GATE read the workspace and wrap up";

/// The hermetic acceptance-target stand-in. It answers the exact envelope contract the
/// harness validates (session identity, turn identity, prompt digest, declared budget,
/// declared terminal outcome) and exits 0, so the drive is a real subprocess drive with
/// zero provider dependency.
fn fake_cli_script(digest: &str) -> String {
    format!(
        r#"#!/bin/sh
# Hermetic stand-in for the acceptance target (issue 143 gate self-tests).
session="s"
turn="1"
prev=""
for arg in "$@"; do
  if [ "$prev" = "--session" ]; then session="$arg"; fi
  if [ "$prev" = "--turn" ]; then turn="$arg"; fi
  prev="$arg"
done
printf '{{"attempt":1,"branch":false,"branch_id":"fake-branch","budget_exhausted":false,"declared_tool_calls":-1,"prompt_digest":"{digest}","replayed":false,"request_attempts":{{"attempts":1,"retries":0}},"session_dir":"/tmp/fake-sessions/%s","session_id":"%s","status":"ok","summary":"CTXEVAL final: CTXEVAL-FINAL-GATE","terminal_outcome":"wrap_up","tool_calls":0,"turn":%s,"zero_call_tail":1}}\n' "$session" "$session" "$turn"
"#
    )
}

fn write_fake_cli(dir: &Path) -> PathBuf {
    let digest = prompt_digest(GATE_PROMPT);
    let path = dir.join("fake-cli.sh");
    fs::write(&path, fake_cli_script(&digest)).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn options_for(root: &Path, out: &Path) -> Options {
    Options {
        eval_root: root.to_path_buf(),
        out_root: out.to_path_buf(),
        runner: RunnerKind::Rust,
        cli: write_fake_cli(root),
        ts_root: PathBuf::new(),
        ts_bin: String::new(),
        allow: Vec::new(),
        records_root: root.join("records"),
    }
}

/// One observed red run record citing `digest` (the manifest bytes that were driven).
fn observed_red(scenario: &str, phase: u8, digest: &str) -> RunRecord {
    RunRecord {
        scenario: scenario.to_string(),
        phase,
        observed_status: "red".to_string(),
        reason_class: "context-limit".to_string(),
        verdict: "expected-red".to_string(),
        runner_revision: "0123456789abcdef".to_string(),
        manifest_digest: digest.to_string(),
        fixture_digests: Vec::new(),
        report: "report-phase-1.json".to_string(),
    }
}

/// One predicted red expectation entry (a prediction is never an observation).
fn predicted_red(scenario: &str, phase: u8) -> StatusRecord {
    StatusRecord {
        scenario: scenario.to_string(),
        phase,
        expected_status: "red".to_string(),
        expected_reason_class: "context-limit".to_string(),
        accept_any_reason: false,
        note: "phase baseline prediction".to_string(),
    }
}

fn history_statuses(root: &Path) -> Vec<(u8, String)> {
    let store = records::Records::new(&root.join("records"));
    records::load_history(&store.history_path())
        .unwrap()
        .into_iter()
        .map(|entry| (entry.phase, entry.expected_status))
        .collect()
}

/// Test 1: scenario driven red at phase 1 (a real observed RunRecord carrying the
/// phase-1 manifest digest), manifest bumped to phase 2 declaring green; the gate
/// accepts, the drive runs, and the phase-2 green observation is recorded.
#[test]
fn green_is_reachable_after_observed_red_in_earlier_phase() {
    let root = tempdir("gate-green");
    let out = out_root("gate-green");
    let scen_dir = root.join("scenarios");
    fs::create_dir_all(&scen_dir).unwrap();

    let phase1 = write_scenario(&scen_dir, &gate_manifest(1, "red", "context-limit"));
    let phase1_digest = crate::context_eval::runner::file_digest(&phase1).unwrap();
    let store = records::Records::new(&root.join("records"));
    store
        .record_run(&observed_red("gate-probe", 1, &phase1_digest))
        .unwrap();
    store.record(&predicted_red("gate-probe", 1)).unwrap();

    // Bump the owning phase and flip the declaration to green: the manifest digest
    // changes with the declaration, which is what makes the phase-1 red citable.
    let green = gate_manifest(2, "green", "none");
    fs::remove_file(&phase1).unwrap();
    let phase2 = write_scenario(&scen_dir, &green);
    assert_ne!(
        crate::context_eval::runner::file_digest(&phase2).unwrap(),
        phase1_digest,
        "the green declaration must be a different manifest state"
    );

    let (aggregate, all_accepted) = run_all(&root, &options_for(&root, &out)).unwrap();
    assert!(all_accepted, "the green drive was not accepted");

    // The append-only history preserved the phase-1 red prediction and appended the
    // phase-2 green declaration; nothing was rewritten.
    assert_eq!(
        history_statuses(&root),
        vec![(1, "red".to_string()), (2, "green".to_string())]
    );

    // The phase-2 observation is a real green RunRecord driven from the new digest.
    let phase2_runs = records::load_runs(&store, 2).unwrap();
    assert_eq!(phase2_runs.len(), 1, "phase-2 run record missing");
    assert_eq!(phase2_runs[0].observed_status, "green");
    assert_eq!(phase2_runs[0].verdict, "pass");
    assert_eq!(
        phase2_runs[0].manifest_digest,
        crate::context_eval::runner::file_digest(&phase2).unwrap()
    );
    // Phase 1 keeps exactly the seeded observed red: the phase-2 drive appended
    // nothing to the earlier phase's append-only file.
    let phase1_runs = records::load_runs(&store, 1).unwrap();
    assert_eq!(phase1_runs.len(), 1, "phase-1 runs were appended to");
    assert_eq!(phase1_runs[0].observed_status, "red");
    assert_eq!(phase1_runs[0].manifest_digest, phase1_digest);
    assert_eq!(aggregate["summary"]["passed"], json!(1));

    cleanup(&root, &out);
}

/// Test 2: a never-driven scenario declaring green (any phase) is still refused: a
/// recorded red *prediction* is not an observed red, so the gate refuses before any
/// subprocess is spawned.
#[test]
fn green_still_requires_a_prior_drive() {
    let root = tempdir("gate-nodrive");
    let out = out_root("gate-nodrive");
    let scen_dir = root.join("scenarios");
    fs::create_dir_all(&scen_dir).unwrap();
    write_scenario(&scen_dir, &gate_manifest(2, "green", "none"));

    let store = records::Records::new(&root.join("records"));
    store.record(&predicted_red("gate-probe", 1)).unwrap();

    let error = run_all(&root, &options_for(&root, &out)).unwrap_err();
    assert!(
        error.contains("no observed red run record"),
        "the gate did not demand an observed red: {error}"
    );
    assert!(
        error.contains("gate-probe"),
        "the refusal lost the scenario id: {error}"
    );

    cleanup(&root, &out);
}

/// Test 3: a cited red recorded under a manifest digest EQUAL to the current manifest's
/// is still refused — the digest cross-check is what keeps a manifest from citing a red
/// driven from its own bytes.
#[test]
fn same_digest_self_citation_still_refused() {
    let root = tempdir("gate-self");
    let out = out_root("gate-self");
    let scen_dir = root.join("scenarios");
    fs::create_dir_all(&scen_dir).unwrap();

    let green = write_scenario(&scen_dir, &gate_manifest(2, "green", "none"));
    let self_digest = crate::context_eval::runner::file_digest(&green).unwrap();

    let store = records::Records::new(&root.join("records"));
    // The prediction trail is satisfied (phase-1 red), so the refusal below is the
    // digest cross-check and nothing else.
    store.record(&predicted_red("gate-probe", 1)).unwrap();
    store
        .record_run(&observed_red("gate-probe", 1, &self_digest))
        .unwrap();

    let error = run_all(&root, &options_for(&root, &out)).unwrap_err();
    assert!(
        error.contains("same manifest digest"),
        "the digest cross-check did not refuse the self-citation: {error}"
    );

    cleanup(&root, &out);
}

/// Test 4: a predicted red StatusRecord licenses nothing. The drive is graded against
/// the observation — never the reverse — so a clean pass fails the red prediction as an
/// unexpected green; the prediction stays a history entry and no red observation is
/// manufactured from it.
#[test]
fn predictions_still_license_nothing() {
    let root = tempdir("gate-prediction");
    let out = out_root("gate-prediction");
    let scen_dir = root.join("scenarios");
    fs::create_dir_all(&scen_dir).unwrap();
    let phase1 = write_scenario(&scen_dir, &gate_manifest(1, "red", "context-limit"));

    let store = records::Records::new(&root.join("records"));
    store.record(&predicted_red("gate-probe", 1)).unwrap();

    let (aggregate, all_accepted) = run_all(&root, &options_for(&root, &out)).unwrap();
    // The fixture drive passes cleanly, so the red prediction fails as an unexpected
    // green: a prediction is graded against the drive, never the reverse.
    assert!(!all_accepted, "an unmet red prediction was accepted");
    assert_eq!(aggregate["summary"]["unexpected_green"], json!(1));

    // The prediction stays a prediction: history carries the red expectation...
    assert_eq!(history_statuses(&root), vec![(1, "red".to_string())]);

    // ...and the only RunRecord is the drive's own observation — an honest green under
    // the phase-1 digest, never a red manufactured from the prediction.
    let runs = records::load_runs(&store, 1).unwrap();
    assert_eq!(runs.len(), 1, "the drive recorded no observation");
    assert_eq!(runs[0].observed_status, "green");
    assert_eq!(runs[0].verdict, "unexpected-green");
    assert_eq!(
        runs[0].manifest_digest,
        crate::context_eval::runner::file_digest(&phase1).unwrap()
    );

    cleanup(&root, &out);
}

/// The scenario-report shape a passing drive publishes, as a validation fixture.
fn good_scenario_report() -> Value {
    json!({
        "id": "gate-probe", "schema_version": 1, "owner_phase": 2, "arm": "feature",
        "expected_status": "green", "runner": "rust", "runner_revision": "abc",
        "fixture_digests": [],
        "profile": {"name": "p", "provider": "openai", "model": "m",
                    "context_limit_tokens": 1000, "max_output_tokens": 100},
        "result": {"verdict": "pass", "accepted": true,
                   "reason_class": "", "failures": []},
        "evidence_status": {"source": "independent", "turns_total": 1, "turns_ok": 1,
                            "provider_requests": 1, "tool_calls_scripted": 0,
                            "final_response_issued": true, "wall_hit": false,
                            "terminal_outcome": "wrap_up", "isolation_ok": true},
        "cache": report::cache_block(),
        "evidence_dimensions": {"task": true, "protocol": true, "resource": true,
                                "latency": true, "recovery": true, "wall_realism": true},
        "request_observations": {"requests": 0, "max_request_bytes": 0,
                                 "streamed_requests": 0, "tool_names": [],
                                 "request_shape_digest": "00",
                                 "observations_source": "loopback"},
        "leakage_scan": {"clean": true, "findings": []},
        "runtime_config": {"name": "profile-limit", "context_limit": 20000},
    })
}

/// Verifier gap: report validation must reject empty nested `profile`/`result`/
/// `evidence_status` objects, a verdict string outside the five names, and an accepted
/// report whose leakage scan did not come back clean.
#[test]
fn empty_nested_objects_unknown_verdicts_and_unclean_leaks_are_rejected() {
    let good = good_scenario_report();
    assert!(
        report::validate(&good, false).is_ok(),
        "valid green scenario report rejected"
    );

    for field in ["profile", "result", "evidence_status"] {
        let mut empty = good.clone();
        empty[field] = json!({});
        let error = report::validate(&empty, false).unwrap_err();
        assert!(
            error.contains("object is empty"),
            "empty {field} object accepted: {error}"
        );
    }

    let mut unknown = good.clone();
    unknown["result"]["verdict"] = json!("pretend-pass");
    let error = report::validate(&unknown, false).unwrap_err();
    assert!(
        error.contains("is not one of"),
        "unknown verdict accepted: {error}"
    );

    let mut leaked = good.clone();
    leaked["leakage_scan"]["clean"] = json!(false);
    let error = report::validate(&leaked, false).unwrap_err();
    assert!(
        error.contains("leakage scan did not come back clean"),
        "an accepted report with an unclean leakage scan passed: {error}"
    );
}

fn write_scenario(dir: &Path, text: &str) -> PathBuf {
    let path = dir.join("gate-probe.toml");
    fs::write(&path, text).unwrap();
    path
}

fn cleanup(root: &Path, out: &Path) {
    fs::remove_dir_all(root).ok();
    fs::remove_dir_all(out).ok();
}

/// The manifest parser must accept the fixture this module drives with (schema guard so
/// a schema change cannot silently strand the lifecycle tests above).
#[test]
fn gate_manifest_fixture_parses_with_declared_outcome() {
    let red = manifest::parse_str(
        &gate_manifest(1, "red", "context-limit"),
        &Path::new("evals/context-management").join("fixtures"),
    )
    .unwrap();
    assert_eq!(red.owner_phase, 1);
    assert_eq!(red.assertions.required_outcomes, vec!["wrap_up"]);
    assert!(red.wall.tool_rounds == 0, "the fixture must stay hermetic");
}
