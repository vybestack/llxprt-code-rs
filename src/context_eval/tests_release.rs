use super::{release, report};
use serde_json::{json, Value};
fn fixture() -> Value {
    let screen = json!({"tool":"llxprt-context-eval","schema_version":report::REPORT_SCHEMA_VERSION,
        "run_id":"fixture","runner":"rust","runner_revision":"c".repeat(40),
        "expected_status_mode":true,"phase0_baseline":null,"records_root":"fixture",
        "scenarios":[],"cache":report::cache_block(),
        "summary":{"total":0,"passed":0,"expected_red":0,"unexpected_green":0,"unexpected_red":0,"harness_error":0}});
    release::assemble(&screen, std::path::Path::new("Cargo.toml"), None).unwrap()
}
#[test]
fn release_fixture_never_claims_unrun_endurance_or_ablations_passed() {
    let v = fixture();
    assert_eq!(v["release_qualified"], false);
    assert_eq!(v["endurance"]["status"], "unavailable");
    for arm in v["arms"].as_array().unwrap() {
        assert_eq!(arm["evidence"]["status"], "unavailable");
    }
    assert!(release::validate(&v).is_ok());
}
#[test]
fn release_completeness_fixture_rejects_every_missing_top_level_field() {
    let v = fixture();
    for field in v.as_object().unwrap().keys() {
        let mut broken = v.clone();
        broken.as_object_mut().unwrap().remove(field);
        assert!(release::validate(&broken).is_err(), "missing {field}");
    }
}
#[test]
fn release_completeness_fixture_rejects_every_missing_inventory_row_or_mapping() {
    let v = fixture();
    for category in ["arms", "operations", "checkpoints"] {
        for index in 0..v[category].as_array().unwrap().len() {
            let mut broken = v.clone();
            broken[category].as_array_mut().unwrap().remove(index);
            assert!(
                release::validate(&broken).is_err(),
                "{category} row {index}"
            );
            for field in v[category][index].as_object().unwrap().keys() {
                let mut broken = v.clone();
                broken[category][index]
                    .as_object_mut()
                    .unwrap()
                    .remove(field);
                assert!(
                    release::validate(&broken).is_err(),
                    "{category} row {index} field {field}"
                );
            }
        }
    }
}
#[test]
fn release_fixture_rejects_threshold_drift_and_fabricated_qualification() {
    let v = fixture();
    for threshold in v["thresholds"].as_object().unwrap().keys() {
        let mut broken = v.clone();
        broken["thresholds"][threshold] = json!(123);
        assert!(release::validate(&broken).is_err(), "threshold {threshold}");
    }
    let mut broken = v.clone();
    broken["release_qualified"] = json!(true);
    assert!(release::validate(&broken).is_err());
    let mut broken = v.clone();
    broken["arms"][0]["evidence"]["status"] = json!("passed");
    assert!(release::validate(&broken).is_err());
}

#[test]
fn issue36_release_inventory_accounts_for_missing_feature_scenarios() {
    let v = fixture();
    let acceptance = &v["architecture_acceptance"];
    assert_eq!(acceptance["tracker_issue"], 36);
    assert_eq!(acceptance["all_features_green"], false);
    assert_eq!(acceptance["baselines_retained"], false);
    assert_eq!(acceptance["features"].as_array().unwrap().len(), 15);
    assert_eq!(acceptance["baselines"].as_array().unwrap().len(), 2);
    for row in acceptance["features"].as_array().unwrap() {
        assert_eq!(row["status"], "missing");
    }
}
