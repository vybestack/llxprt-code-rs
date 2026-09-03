//! Versioned report schema for context evals.
//! Cache accounting is loaded from the runtime-owned rewrite journal when available.

use serde_json::{json, Value};
use std::path::Path;

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
        "rewrite_journal_tokens_reclaimed": Value::Null,
        "rewrite_journal_tokens_invalidated": Value::Null,
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
    let mut tokens_reclaimed = 0_u64;
    let mut tokens_invalidated = 0_u64;
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
        let Some(reclaimed) = value.get("tokens_reclaimed").and_then(Value::as_u64) else {
            continue;
        };
        entries = entries.saturating_add(1);
        tokens_reclaimed = tokens_reclaimed.saturating_add(reclaimed);
        match value.get("invalidation_cost").and_then(Value::as_u64) {
            Some(cost) => tokens_invalidated = tokens_invalidated.saturating_add(cost),
            None => unknown_invalidation = true,
        }
    }
    let Some(report) = runtime_report else {
        return cache_block();
    };
    let invalidated = if unknown_invalidation {
        Value::Null
    } else {
        json!(tokens_invalidated)
    };
    json!({
        "class": "measured",
        "hit_rate": report["hit_rate"],
        "armed_hit_rate": report["armed_hit_rate"],
        "disarmed_hit_rate": report["disarmed_hit_rate"],
        "invalidation_cost_per_event": report["invalidation_cost_per_event"],
        "prefix_invalidation_cost_per_rewrite": report["invalidation_cost_per_event"],
        "known_invalidation_cost_events": report["known_invalidation_cost_events"],
        "unknown_invalidation_cost_events": report["unknown_invalidation_cost_events"],
        "rewrite_journal_entries": entries,
        "rewrite_journal_tokens_reclaimed": tokens_reclaimed,
        "rewrite_journal_tokens_invalidated": invalidated,
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
        "suspended_while_armed": report["armed_rewrites"].as_u64().unwrap_or(0) > 0,
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
            cache["rewrite_journal_tokens_reclaimed"]
                .as_u64()
                .unwrap_or(0),
        )
    });
    json!({
        "class": "measured_aggregate",
        "hit_rate": Value::Null,
        "prefix_invalidation_cost_per_rewrite": Value::Null,
        "rewrite_journal_tokens_reclaimed": reclaimed,
        "rewrite_journal_tokens_invalidated": Value::Null,
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
