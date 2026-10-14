//! Issue 36 behavioral acceptance inventory, distinct from expected-status screening.
//! Missing scenarios and accepted reds are blockers, never architecture qualification.
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub(super) const FEATURES: &[&str] = &[
    "wall-large-tool-final",
    "wall-followup-recovery",
    "wall-mixed-lanes",
    "terminal-reserve-wrap-up",
    "quiesce-unwritable",
    "legality-pairing-and-quoting",
    "ingress-secret-and-digest",
    "budget-governor-progress",
    "fact-conflict-restart",
    "gate-recurrent-rooting",
    "provider-crash-matrix",
    "branch-readset-conflict",
    "security-authority-laundering",
    "epoch-cache",
    "endurance-restart",
];
pub(super) const BASELINES: &[(&str, &str)] = &[
    ("baseline-status-quo-full-replay", "status-quo"),
    ("baseline-minimum-management-floor", "minimum-floor"),
];

pub(super) fn assemble(screen: &Value) -> Result<Value, String> {
    super::report::validate(screen, true)?;
    let scenarios = screen["scenarios"].as_array().ok_or("missing scenarios")?;
    let members: BTreeMap<&str, &Value> = scenarios
        .iter()
        .map(|s| Ok((s["id"].as_str().ok_or("missing scenario id")?, s)))
        .collect::<Result<_, String>>()?;
    let features: Vec<Value> = FEATURES
        .iter()
        .map(|id| feature(id, members.get(id).copied(), screen["runner"] == "rust"))
        .collect();
    let baselines: Vec<Value> = BASELINES
        .iter()
        .map(|(id, arm)| {
            let member = members.get(id).copied();
            let retained = member.is_some_and(|s| s["arm"] == *arm);
            json!({"id":id,"arm":arm,"retained":retained,
                "verdict":member.map(|s| s["result"]["verdict"].clone())})
        })
        .collect();
    let unrecognized: Vec<&str> = members
        .keys()
        .copied()
        .filter(|id| !FEATURES.contains(id) && !BASELINES.iter().any(|(b, _)| b == id))
        .collect();
    let all_green = features.iter().all(|f| f["status"] == "green");
    let baselines_retained =
        screen["runner"] == "rust" && baselines.iter().all(|b| b["retained"] == true);
    let screen_passed = all_green && baselines_retained && unrecognized.is_empty();
    Ok(json!({
        "tracker_issue":36,"candidate_sha":screen["runner_revision"],
        "all_features_green":all_green,"baselines_retained":baselines_retained,
        "behavioral_screen_passed":screen_passed,
        "features":features,"baselines":baselines,"unrecognized_scenarios":unrecognized,
        "limitation":"behavioral screen only; predecessor red provenance, composed invariant checkpoints, security comparisons, benchmark pins and >=48-hour endurance still require independent evidence; not architecture qualification",
    }))
}

fn feature(id: &str, member: Option<&Value>, rust: bool) -> Value {
    let Some(scenario) = member else {
        return json!({"id":id,"status":"missing","blockers":["scenario not executed"]});
    };
    let mut blockers = Vec::new();
    for (ok, reason) in [
        (rust, "reference runner is not the Rust acceptance target"),
        (
            scenario["arm"] == "feature",
            "scenario is not a feature arm",
        ),
        (
            scenario["expected_status"] == "green",
            "manifest is not green",
        ),
        (
            scenario["result"]["verdict"] == "pass",
            "observed verdict is not pass",
        ),
        (
            scenario["evidence_status"]["wall_hit"] == false,
            "wall hit or unknown wall evidence",
        ),
        (
            scenario["evidence_status"]["isolation_ok"] == true,
            "isolation not established",
        ),
    ] {
        if !ok {
            blockers.push(reason.to_string());
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
        if scenario["evidence_dimensions"][dim] != true {
            blockers.push(format!("{dim} dimension failed"));
        }
    }
    let status = if blockers.is_empty() {
        "green"
    } else {
        "blocked"
    };
    json!({"id":id,"status":status,
        "verdict":scenario["result"]["verdict"],"wall_hit":scenario["evidence_status"]["wall_hit"],
        "blockers":blockers})
}
