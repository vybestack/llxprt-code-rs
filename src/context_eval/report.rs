//! Versioned report schema for context evals (#37).
//!
//! Every per-scenario and aggregate report carries the cache-stat fields now, marked with
//! the unknown cost class `disarmed_unavailable`: the fields exist in Phase 0, their
//! values are unknown until Phase 4 supplies cache telemetry. The schema validator is the
//! same code the harness self-tests use to prove malformed reports are detected.

use serde_json::{json, Value};

/// Report schema version. Bumping it is a breaking eval change.
pub const REPORT_SCHEMA_VERSION: u32 = 1;
/// Cost class recorded when cache telemetry is unavailable.
pub const CACHE_UNKNOWN_CLASS: &str = "disarmed_unavailable";

/// Fields every per-scenario report must carry.
pub const SCENARIO_REQUIRED: [&str; 12] = [
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
];

/// Fields every aggregate report must carry.
pub const AGGREGATE_REQUIRED: [&str; 8] = [
    "tool",
    "schema_version",
    "run_id",
    "runner",
    "expected_status_mode",
    "scenarios",
    "summary",
    "cache",
];

/// The Phase 0 cache block: fields present, values unknown.
pub fn cache_block() -> Value {
    json!({
        "class": CACHE_UNKNOWN_CLASS,
        "hit_rate": Value::Null,
        "prefix_invalidation_cost_per_rewrite": Value::Null,
        "rewrite_journal_tokens_reclaimed": Value::Null,
        "rewrite_journal_tokens_invalidated": Value::Null,
        "amortization_decisions_below_at_above": Value::Null,
        "suspended_while_armed": Value::Null,
        "source": "unknown",
        "note": "fields exist in Phase 0; values unknown until Phase 4 telemetry",
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
    let class = value["cache"]["class"].as_str();
    if class != Some(CACHE_UNKNOWN_CLASS) && value["cache"]["class"].is_null() {
        return Err("report cache block has no cost class".to_string());
    }
    if aggregate {
        validate_summary(&value["summary"])?;
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
