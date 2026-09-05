//! Versioned report schema for context evals.
//! Cache accounting is loaded from the runtime-owned rewrite journal when available.

use serde_json::{json, Value};
use std::path::Path;

use crate::context_eval::REQUEST_SHAPE_DIGEST_KEY;

/// Report schema version. Bumping it is a breaking eval change.
pub const REPORT_SCHEMA_VERSION: u32 = 1;
/// Cost class recorded when cache telemetry is unavailable.
pub const CACHE_UNKNOWN_CLASS: &str = "disarmed_unavailable";
/// Every verdict name the schema accepts, exactly as [`crate::context_eval::grader::Verdict`]
/// names them. A report carrying anything else is unpublishable.
pub const VERDICT_NAMES: [&str; 5] = [
    "pass",
    "expected-red",
    "unexpected-green",
    "unexpected-red-reason",
    "harness-error",
];
/// Every reason class the grader can name. `leakage` is deliberately absent from the
/// "acceptable red" story a report may carry, which is what makes a leak gate rather
/// than merely label.
pub const REASON_CLASS_NAMES: [&str; 7] = [
    "harness-error",
    "leakage",
    "context-limit",
    "resource-limit",
    "recovery-failure",
    "missing-evidence",
    "task-failure",
];

/// Fields every per-scenario report must carry.
pub const SCENARIO_REQUIRED: [&str; 16] = [
    "id",
    "schema_version",
    "owner_phase",
    "arm",
    "expected_status",
    "runner",
    "runner_revision",
    "fixture_digests",
    "profile",
    "result",
    "evidence_status",
    "cache",
    "runtime_config",
    "evidence_dimensions",
    "request_observations",
    "leakage_scan",
];

/// Fields every aggregate report must carry.
pub const AGGREGATE_REQUIRED: [&str; 11] = [
    "tool",
    "schema_version",
    "run_id",
    "runner",
    "runner_revision",
    "expected_status_mode",
    "phase0_baseline",
    "records_root",
    "scenarios",
    "summary",
    "cache",
];

/// Fields the nested `result` object must carry (a bare `{}` is not a result).
const RESULT_REQUIRED: [&str; 4] = ["verdict", "accepted", "reason_class", "failures"];

/// Fields the nested `profile` object must carry (a bare `{}` is not a profile).
const PROFILE_REQUIRED: [&str; 5] = [
    "name",
    "provider",
    "model",
    "context_limit_tokens",
    "max_output_tokens",
];

/// Fields the nested `evidence_status` object must carry.
const EVIDENCE_STATUS_REQUIRED: [&str; 9] = [
    "source",
    "turns_total",
    "turns_ok",
    "provider_requests",
    "tool_calls_scripted",
    "final_response_issued",
    "wall_hit",
    "terminal_outcome",
    "isolation_ok",
];

/// Cache telemetry is explicitly unknown when no durable journal is available.
pub fn cache_block() -> Value {
    json!({
        "class": CACHE_UNKNOWN_CLASS,
        "hit_rate": Value::Null,
        "armed_hit_rate": Value::Null,
        "disarmed_hit_rate": Value::Null,
        "invalidation_cost_per_event": Value::Null,
        "prefix_invalidation_cost_per_rewrite": Value::Null,
        "rewrite_journal_entries": Value::Null,
        "rewrite_journal_bytes_reclaimed": Value::Null,
        "rewrite_journal_bytes_invalidated": Value::Null,
        "amortization_decisions_below_at_above": Value::Null,
        "suspended_while_armed": Value::Null,
        "source": "unknown",
        "note": "runtime rewrite-journal telemetry was unavailable",
    })
}

/// Load an acceptance-report cache block from the runtime rewrite journal.
pub fn cache_block_from_session(session_dir: Option<&Path>) -> Value {
    let Some(session_dir) = session_dir else {
        return cache_block();
    };
    let Ok(text) = std::fs::read_to_string(session_dir.join("context/rewrite-journal.log")) else {
        return cache_block();
    };
    let mut entries = 0_u64;
    let mut bytes_reclaimed = 0_u64;
    let mut bytes_invalidated = 0_u64;
    let mut unknown_invalidation = false;
    let mut runtime_report = None;
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return cache_block();
        };
        if let Some(report) = value.get("report") {
            runtime_report = Some(report.clone());
            continue;
        }
        let Some(reclaimed) = value.get("bytes_reclaimed").and_then(Value::as_u64) else {
            continue;
        };
        entries = entries.saturating_add(1);
        bytes_reclaimed = bytes_reclaimed.saturating_add(reclaimed);
        match value.get("invalidation_cost").and_then(Value::as_u64) {
            Some(cost) => bytes_invalidated = bytes_invalidated.saturating_add(cost),
            None => unknown_invalidation = true,
        }
    }
    let Some(report) = runtime_report else {
        return cache_block();
    };
    // F17: the sibling field is byte-labelled, so this one keeps the same
    // unit across journal and report; a missing cost stays unknown rather
    // than being mislabelled as a measured byte count.
    let invalidated = if unknown_invalidation {
        Value::Null
    } else {
        json!(bytes_invalidated)
    };
    json!({
        "class": "measured",
        "hit_rate": report["hit_rate"],
        "armed_hit_rate": report["armed_hit_rate"],
        "disarmed_hit_rate": report["disarmed_hit_rate"],
        "invalidation_cost_per_event": report["invalidation_cost_per_event"],
        "prefix_invalidation_cost_per_rewrite": Value::Null,
        "known_invalidation_cost_events": report["known_invalidation_cost_events"],
        "unknown_invalidation_cost_events": report["unknown_invalidation_cost_events"],
        "rewrite_journal_entries": entries,
        "rewrite_journal_bytes_reclaimed": bytes_reclaimed,
        "rewrite_journal_bytes_invalidated": invalidated,
        "amortization_decisions_below_at_above": {
            "below": report["threshold_denials"],
            "at_or_above": report["threshold_passes"],
        },
        "conditional": {
            "armed_rewrites": report["armed_rewrites"],
            "disarmed_rewrites": report["disarmed_rewrites"],
            "armed_hit_rate": report["armed_hit_rate"],
            "disarmed_hit_rate": report["disarmed_hit_rate"],
        },
        "suspended_while_armed": report["economic_gate_suspensions"].as_u64().unwrap_or(0) > 0,
        "source": "context/rewrite-journal.log",
    })
}

/// Preserve each scenario's conditional report in the aggregate acceptance report.
pub fn aggregate_cache(scenarios: &[Value]) -> Value {
    let measured: Vec<Value> = scenarios
        .iter()
        .filter_map(|scenario| scenario.get("cache"))
        .filter(|cache| cache.get("class").and_then(Value::as_str) == Some("measured"))
        .cloned()
        .collect();
    if measured.is_empty() {
        return cache_block();
    }
    let reclaimed = measured.iter().fold(0_u64, |sum, cache| {
        sum.saturating_add(
            cache["rewrite_journal_bytes_reclaimed"]
                .as_u64()
                .unwrap_or(0),
        )
    });
    json!({
        "class": "measured_aggregate",
        "hit_rate": Value::Null,
        "prefix_invalidation_cost_per_rewrite": Value::Null,
        "rewrite_journal_bytes_reclaimed": reclaimed,
        "rewrite_journal_bytes_invalidated": Value::Null,
        "amortization_decisions_below_at_above": Value::Null,
        "suspended_while_armed": measured.iter().any(|cache| cache["suspended_while_armed"] == true),
        "source": "scenario rewrite journals",
        "conditional_reports": measured,
    })
}

/// Validate a report value against the required schema. `aggregate` selects the field set.
pub fn validate(value: &Value, aggregate: bool) -> Result<(), String> {
    let fields: &[&str] = if aggregate {
        &AGGREGATE_REQUIRED
    } else {
        &SCENARIO_REQUIRED
    };
    let mut missing = Vec::new();
    for name in fields {
        if value.get(name).is_none() {
            missing.push(name);
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "report is missing required fields: {}",
            missing
                .iter()
                .map(|s| **s)
                .collect::<Vec<&str>>()
                .join(", ")
        ));
    }
    if value["schema_version"].as_u64() != Some(REPORT_SCHEMA_VERSION.into()) {
        return Err("report schema_version does not match the harness".to_string());
    }
    if value["cache"]["class"].is_null() {
        return Err("report cache block has no cost class".to_string());
    }
    if !aggregate {
        validate_nested_objects(value)?;
        let dims = value["evidence_dimensions"]
            .as_object()
            .ok_or("report has no evidence_dimensions object")?;
        for dim in [
            "task",
            "protocol",
            "resource",
            "latency",
            "recovery",
            "wall_realism",
        ] {
            if dims.get(dim).and_then(Value::as_bool).is_none() {
                return Err(format!("evidence_dimensions is missing boolean {dim}"));
            }
        }
        let obs = value["request_observations"]
            .as_object()
            .ok_or("report has no request_observations object")?;
        for field in [
            "requests",
            "max_request_bytes",
            "streamed_requests",
            "tool_names",
            REQUEST_SHAPE_DIGEST_KEY,
            "observations_source",
        ] {
            if obs.get(field).is_none() {
                return Err(format!("request_observations is missing {field}"));
            }
        }
        validate_leakage_scan(value)?;
    }
    if aggregate {
        validate_summary(&value["summary"])?;
    }
    Ok(())
}

/// Reject empty nested objects: `profile`, `result`, and `evidence_status` must each be a
/// populated object, and `result.verdict` must be one of the five verdict names.
///
/// The old check accepted `"profile": {}` and any verdict string, which is exactly how a
/// harness change could silently publish a report whose graded content was absent.
fn validate_nested_objects(value: &Value) -> Result<(), String> {
    for (name, required) in [
        ("profile", &PROFILE_REQUIRED[..]),
        ("evidence_status", &EVIDENCE_STATUS_REQUIRED[..]),
    ] {
        let object = value[name]
            .as_object()
            .ok_or(format!("report {name} is not an object"))?;
        if object.is_empty() {
            return Err(format!("report {name} object is empty"));
        }
        for field in required {
            if !object.contains_key(*field) {
                return Err(format!("report {name} is missing {field}"));
            }
        }
    }
    let result = value["result"]
        .as_object()
        .ok_or("report result is not an object")?;
    if result.is_empty() {
        return Err("report result object is empty".to_string());
    }
    for field in RESULT_REQUIRED {
        if !result.contains_key(field) {
            return Err(format!("report result is missing {field}"));
        }
    }
    let verdict = result["verdict"]
        .as_str()
        .ok_or("report result verdict is not a string")?;
    if !VERDICT_NAMES.contains(&verdict) {
        return Err(format!(
            "report result verdict {verdict} is not one of {}",
            VERDICT_NAMES.join(", ")
        ));
    }
    let accepted = result["accepted"]
        .as_bool()
        .ok_or("report result accepted is not a boolean")?;
    let verdict_accepted = matches!(verdict, "pass" | "expected-red");
    if accepted != verdict_accepted {
        return Err(format!(
            "report result accepted {accepted} disagrees with verdict {verdict}"
        ));
    }
    let reason = result["reason_class"]
        .as_str()
        .ok_or("report result reason_class is not a string")?;
    if !REASON_CLASS_NAMES.contains(&reason) {
        return Err(format!(
            "report result reason_class {reason} is not a reason class the grader can name"
        ));
    }
    let failures = result["failures"]
        .as_array()
        .ok_or("report result failures is not an array")?;
    if accepted && !failures.is_empty() {
        return Err("report result accepted a verdict that still carries failures".to_string());
    }
    // A pass has no failure to name; a green observation carrying a failure reason class
    // is a record contradiction, not a graded result.
    if verdict == "pass" && !reason.is_empty() {
        return Err("report result pass carries a failure reason class".to_string());
    }
    Ok(())
}

/// The leakage scan gates acceptance (#116.2/6): `clean` must be an explicit boolean and
/// a report may only be accepted when the scan actually ran and found nothing.
fn validate_leakage_scan(value: &Value) -> Result<(), String> {
    let leak = value["leakage_scan"]
        .as_object()
        .ok_or("report has no leakage_scan object")?;
    let findings = leak
        .get("findings")
        .and_then(Value::as_array)
        .ok_or("leakage_scan must carry a findings array")?;
    let clean = leak["clean"].as_bool();
    let Some(clean) = clean else {
        return Err("leakage_scan must carry boolean clean".to_string());
    };
    if !findings.is_empty() && clean {
        return Err("leakage_scan claims clean while carrying findings".to_string());
    }
    let accepted = value["result"]["accepted"].as_bool().unwrap_or(false);
    if accepted && !clean {
        return Err(
            "leakage scan did not come back clean; a leak blocks acceptance for every \
             scenario"
                .to_string(),
        );
    }
    let status = leak.get("status").and_then(Value::as_str);
    match status {
        // A scan that ran may omit `status` (the shipped shape) or name `performed`.
        None | Some("performed") => {
            if !leak.get("markers").map(|m| m.is_array()).unwrap_or(true) {
                return Err("leakage_scan markers must be an array".to_string());
            }
        }
        // A scan that did not run must say so and must not claim clean.
        Some("not_performed") => {
            if clean {
                return Err(
                    "leakage_scan marks the scan not_performed but still claims clean".to_string(),
                );
            }
        }
        Some(other) => {
            return Err(format!(
                "leakage_scan status {other} is neither performed nor not_performed"
            ));
        }
    }
    Ok(())
}

fn validate_summary(summary: &Value) -> Result<(), String> {
    for key in [
        "total",
        "expected_red",
        "unexpected_green",
        "unexpected_red",
        "harness_error",
    ] {
        if summary.get(key).and_then(Value::as_u64).is_none() {
            return Err(format!("aggregate summary is missing the {key} count"));
        }
    }
    Ok(())
}
