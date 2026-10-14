use super::{
    report,
    tests::{good_aggregate_report, good_report},
};
use serde_json::json;

#[test]
fn phase9_release_report_rejects_unreconciled_cache() {
    let mut scenario = good_report();
    scenario["cache"] = json!({"class":"measured", "source":"fixture", "epochs":[
        {"epoch":0,"trigger":"budget_pressure","invalidations":2}
    ]});
    assert!(
        report::validate(&scenario, false).is_err(),
        "two invalidations for one cut accepted"
    );
}

#[test]
fn phase9_release_report_validates_every_aggregate_member() {
    let mut aggregate = good_aggregate_report();
    aggregate["scenarios"] = json!([good_report()]);
    aggregate["scenarios"][0]
        .as_object_mut()
        .unwrap()
        .remove("cache");
    aggregate["summary"]["total"] = json!(1);
    aggregate["summary"]["expected_red"] = json!(1);
    assert!(
        report::validate(&aggregate, true).is_err(),
        "member without cache accepted"
    );
}

#[test]
fn phase9_expected_red_preserves_actual_failure_evidence_but_pass_cannot() {
    let mut expected_red = good_report();
    expected_red["result"]["failures"] = json!(["feature not landed"]);
    assert!(report::validate(&expected_red, false).is_ok());
    expected_red["result"]["verdict"] = json!("pass");
    assert!(report::validate(&expected_red, false).is_err());
}

#[test]
fn phase9_corrupt_epoch_artifact_is_not_reclassified_as_unknown_telemetry() {
    let root = tempfile::tempdir().unwrap();
    let dir = crate::session::context_artifact_dir(root.path());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("epochs.json"), b"{truncated").unwrap();
    let mut scenario = good_report();
    scenario["cache"] = report::cache_block_from_session(Some(root.path()));
    assert_eq!(scenario["cache"]["class"], "corrupt");
    assert!(report::validate(&scenario, false).is_err());
}

#[test]
fn phase9_aggregate_rejects_duplicate_scenarios_and_invented_summary_counts() {
    let mut scenario = good_report();
    scenario["result"]["failures"] = json!(["feature not landed"]);
    let mut aggregate = good_aggregate_report();
    aggregate["scenarios"] = json!([scenario.clone()]);
    aggregate["cache"] = report::aggregate_cache(aggregate["scenarios"].as_array().unwrap());
    aggregate["summary"]["total"] = json!(1);
    aggregate["summary"]["expected_red"] = json!(1);
    assert!(report::validate(&aggregate, true).is_ok());
    aggregate["summary"]["total"] = json!(2);
    assert!(report::validate(&aggregate, true).is_err());
    aggregate["scenarios"] = json!([scenario.clone(), scenario]);
    aggregate["cache"] = report::aggregate_cache(aggregate["scenarios"].as_array().unwrap());
    aggregate["summary"]["expected_red"] = json!(2);
    assert!(report::validate(&aggregate, true).is_err());
}

#[test]
fn aggregate_cache_overflow_is_a_harness_error_not_a_zero_or_unknown_price() {
    let mut cache = crate::context_policy::cache::EpochCache::default();
    cache.start("a".repeat(64), 4).unwrap();
    cache.observe(0, Some(u64::MAX), Some(0), false).unwrap();
    let root = tempfile::tempdir().unwrap();
    let dir = crate::session::context_artifact_dir(root.path());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("epochs.json"), serde_json::to_vec(&cache).unwrap()).unwrap();
    let mut a = good_report();
    a["cache"] = report::cache_block_from_session(Some(root.path()));
    report::validate(&a, false).unwrap();
    let mut b = a.clone();
    b["id"] = json!("distinct");
    let aggregate = report::aggregate_cache(&[a, b]);
    assert_eq!(aggregate["class"], "corrupt");
    assert!(super::cache_report::validate(&aggregate).is_err());
}

#[test]
fn grader_requires_current_committed_generation_not_stale_flat_artifacts() {
    let scenarios = super::manifest::load_dir(
        std::path::Path::new("evals/context-management/scenarios"),
        std::path::Path::new("evals/context-management/fixtures"),
    )
    .unwrap();
    let scenario = &scenarios
        .iter()
        .find(|(_, s)| s.id == "terminal-reserve-wrap-up")
        .unwrap()
        .1;
    let root = tempfile::tempdir().unwrap();
    let flat = root.path().join("context");
    let committed = flat.join("committed");
    std::fs::create_dir_all(&committed).unwrap();
    let ev = super::grader::Evidence {
        session_dir: Some(root.path().to_path_buf()),
        ..Default::default()
    };
    for name in ["events.log", "checkpoints"] {
        std::fs::write(flat.join(name), b"stale").unwrap();
    }
    let before = super::grader::grade(scenario, &ev);
    assert_eq!(
        before
            .failures
            .iter()
            .filter(|f| f.contains("context artifact absent"))
            .count(),
        2
    );
    for name in ["events.log", "checkpoints"] {
        std::fs::write(committed.join(name), b"fixture").unwrap();
    }
    let after = super::grader::grade(scenario, &ev);
    assert_eq!(
        after
            .failures
            .iter()
            .filter(|f| f.contains("context artifact absent"))
            .count(),
        0
    );
    assert!(
        !after.passed,
        "artifact existence does not prove task success"
    );
}

#[test]
fn issue36_aggregate_rejects_mixed_candidate_or_runner_evidence() {
    let scenario = good_report();
    let mut aggregate = good_aggregate_report();
    aggregate["scenarios"] = json!([scenario]);
    aggregate["summary"]["total"] = json!(1);
    aggregate["summary"]["expected_red"] = json!(1);
    aggregate["cache"] = report::aggregate_cache(aggregate["scenarios"].as_array().unwrap());
    report::validate(&aggregate, true).unwrap();
    for (field, foreign) in [("runner_revision", "other-head"), ("runner", "typescript-reference")] {
        let mut mixed = aggregate.clone();
        mixed["scenarios"][0][field] = json!(foreign);
        assert!(report::validate(&mixed, true).is_err(), "mixed {field} accepted");
    }
}
