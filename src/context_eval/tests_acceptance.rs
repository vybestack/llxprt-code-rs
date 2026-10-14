//! Structural acceptance tests; synthetic green fixtures are not live evidence.
use super::{
    acceptance, release, report,
    tests::{good_aggregate_report, good_report},
};
use serde_json::{json, Value};

fn full_screen() -> Value {
    let mut screen = good_aggregate_report();
    screen["runner_revision"] = json!("c".repeat(40));
    let mut scenarios = Vec::new();
    for id in acceptance::FEATURES
        .iter()
        .copied()
        .chain(acceptance::BASELINES.iter().map(|(id, _)| *id))
    {
        let mut scenario = good_report();
        scenario["id"] = json!(id);
        scenario["runner_revision"] = screen["runner_revision"].clone();
        scenario["expected_status"] = json!("green");
        scenario["result"] =
            json!({"verdict":"pass", "accepted":true, "reason_class":"", "failures":[]});
        scenario["evidence_status"]["wall_hit"] = json!(false);
        if let Some((_, arm)) = acceptance::BASELINES.iter().find(|(b, _)| *b == id) {
            scenario["arm"] = json!(arm);
        }
        scenarios.push(scenario);
    }
    screen["summary"]["total"] = json!(scenarios.len());
    screen["summary"]["passed"] = json!(scenarios.len());
    screen["cache"] = report::aggregate_cache(&scenarios);
    screen["scenarios"] = json!(scenarios);
    screen
}

#[test]
fn issue36_inventory_matches_the_committed_phase0_catalog() {
    let scenarios = super::manifest::load_dir(
        std::path::Path::new("evals/context-management/scenarios"),
        std::path::Path::new("evals/context-management/fixtures"),
    )
    .unwrap();
    let actual: std::collections::BTreeSet<_> =
        scenarios.iter().map(|(_, s)| s.id.as_str()).collect();
    let expected = acceptance::FEATURES
        .iter()
        .copied()
        .chain(acceptance::BASELINES.iter().map(|(id, _)| *id))
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn issue36_every_missing_feature_and_baseline_blocks_the_full_screen() {
    let screen = full_screen();
    assert_eq!(
        acceptance::assemble(&screen).unwrap()["behavioral_screen_passed"],
        true
    );
    for index in 0..screen["scenarios"].as_array().unwrap().len() {
        let mut partial = screen.clone();
        partial["scenarios"].as_array_mut().unwrap().remove(index);
        partial["summary"]["total"] = json!(16);
        partial["summary"]["passed"] = json!(16);
        partial["cache"] = report::aggregate_cache(partial["scenarios"].as_array().unwrap());
        assert_eq!(
            acceptance::assemble(&partial).unwrap()["behavioral_screen_passed"],
            false
        );
    }
}

#[test]
fn issue36_expected_red_is_accepted_screening_but_not_feature_acceptance() {
    let mut screen = full_screen();
    screen["scenarios"][0]["expected_status"] = json!("red");
    screen["scenarios"][0]["result"] = json!({"verdict":"expected-red", "accepted":true,
        "reason_class":"missing-evidence", "failures":["not implemented"]});
    screen["summary"]["passed"] = json!(16);
    screen["summary"]["expected_red"] = json!(1);
    report::validate(&screen, true).unwrap();
    let v = acceptance::assemble(&screen).unwrap();
    assert_eq!(v["features"][0]["status"], "blocked");
    assert_eq!(v["all_features_green"], false);
}

#[test]
fn issue36_green_verdict_cannot_hide_wall_dimension_or_isolation_failure() {
    let screen = full_screen();
    for field in ["wall_hit", "isolation_ok"] {
        for bad in [json!(null), json!("unknown"), json!(field == "wall_hit")] {
            let mut broken = screen.clone();
            broken["scenarios"][0]["evidence_status"][field] = bad;
            assert_eq!(
                acceptance::assemble(&broken).unwrap()["all_features_green"],
                false
            );
        }
    }
    for dim in [
        "task",
        "protocol",
        "resource",
        "latency",
        "recovery",
        "wall_realism",
    ] {
        let mut broken = screen.clone();
        broken["scenarios"][0]["evidence_dimensions"][dim] = json!(false);
        assert_eq!(
            acceptance::assemble(&broken).unwrap()["all_features_green"],
            false
        );
    }
    let mut reference = screen.clone();
    reference["runner"] = json!("typescript-reference");
    for s in reference["scenarios"].as_array_mut().unwrap() {
        s["runner"] = json!("typescript-reference");
    }
    assert_eq!(
        acceptance::assemble(&reference).unwrap()["behavioral_screen_passed"],
        false
    );
}

#[test]
fn issue36_release_acceptance_is_recomputed_not_trusted_or_promoted() {
    let release =
        release::assemble(&full_screen(), std::path::Path::new("Cargo.toml"), None).unwrap();
    assert_eq!(
        release["architecture_acceptance"]["behavioral_screen_passed"],
        true
    );
    assert_eq!(release["release_qualified"], false);
    for field in release["architecture_acceptance"]
        .as_object()
        .unwrap()
        .keys()
    {
        let mut broken = release.clone();
        broken["architecture_acceptance"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(release::validate(&broken).is_err(), "missing {field}");
    }
    for index in 0..15 {
        let mut broken = release.clone();
        broken["architecture_acceptance"]["features"][index]["status"] = json!("missing");
        assert!(release::validate(&broken).is_err());
    }
    let mut invented = release.clone();
    invented["architecture_acceptance"]["tracker_issue"] = json!(46);
    assert!(release::validate(&invented).is_err());
}

#[test]
fn issue36_unknown_scenarios_and_relabelled_arms_do_not_cover_the_catalog() {
    let screen = full_screen();
    let mut renamed = screen.clone();
    renamed["scenarios"][0]["id"] = json!("wall-lookalike");
    let v = acceptance::assemble(&renamed).unwrap();
    assert_eq!(v["features"][0]["status"], "missing");
    assert_eq!(v["unrecognized_scenarios"], json!(["wall-lookalike"]));
    assert_eq!(v["behavioral_screen_passed"], false);
    let mut wrong_arm = screen.clone();
    wrong_arm["scenarios"][0]["arm"] = json!("status-quo");
    assert_eq!(
        acceptance::assemble(&wrong_arm).unwrap()["all_features_green"],
        false
    );
    wrong_arm = screen;
    wrong_arm["scenarios"][15]["arm"] = json!("feature");
    assert_eq!(
        acceptance::assemble(&wrong_arm).unwrap()["baselines_retained"],
        false
    );
}
