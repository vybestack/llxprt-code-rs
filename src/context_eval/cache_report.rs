//! Strict epoch/cut reconciliation for scenario and aggregate acceptance output.
use crate::context_policy::cache::{EpochCache, EpochRecord};
use serde_json::{json, Value};

pub fn validate(value: &Value) -> Result<(), String> {
    match value["class"].as_str() {
        Some("unavailable") => {
            if value["source"] != "unknown"
                || !value["epochs"].is_null()
                || !value["hit_rate"].is_null()
            {
                return Err("unavailable cache must not fabricate telemetry".into());
            }
        }
        Some("measured") => {
            let epochs = value["epochs"].as_array().ok_or("cache has no epochs")?;
            let records = epochs
                .iter()
                .map(validate_epoch)
                .collect::<Result<Vec<_>, _>>()?;
            let cache: EpochCache = serde_json::from_value(json!({"records": records}))
                .map_err(|e| format!("cache accounting: {e}"))?;
            cache
                .validate()
                .map_err(|e| format!("cache accounting: {e}"))?;
            if value["source"].as_str().is_none_or(str::is_empty) {
                return Err("cache telemetry has no source".into());
            }
        }
        Some("aggregate") => {
            let reports = value["conditional_reports"]
                .as_array()
                .ok_or("cache aggregate has no reports")?;
            for report in reports {
                validate(report)?;
            }
            if value["totals"] != totals(reports)? {
                return Err("aggregate cache subtotals do not reconcile".into());
            }
        }
        _ => return Err("cache cost class is invalid or corrupt".into()),
    }
    Ok(())
}

pub fn validate_members(value: &Value) -> Result<(), String> {
    let scenarios = value["scenarios"]
        .as_array()
        .ok_or("aggregate has no scenarios array")?;
    let mut ids = std::collections::BTreeSet::new();
    let mut counts = [0u64; 5];
    for scenario in scenarios {
        super::report::validate(scenario, false)?;
        let id = scenario["id"].as_str().ok_or("scenario has no id")?;
        if !ids.insert(id) {
            return Err(format!("duplicate scenario {id}"));
        }
        let index = super::report::VERDICT_NAMES
            .iter()
            .position(|v| scenario["result"]["verdict"] == *v)
            .ok_or("invalid verdict")?;
        counts[index] += 1;
    }
    for (key, count) in [
        "passed",
        "expected_red",
        "unexpected_green",
        "unexpected_red",
        "harness_error",
    ]
    .iter()
    .zip(counts)
    {
        if value["summary"][key].as_u64() != Some(count) {
            return Err(format!("aggregate {key} does not reconcile"));
        }
    }
    if value["summary"]["total"].as_u64() != Some(scenarios.len() as u64) {
        return Err("aggregate total does not reconcile".into());
    }
    let expected = super::report::aggregate_cache(scenarios);
    if value["cache"] != expected {
        return Err("aggregate cache does not reconcile to scenarios".into());
    }
    Ok(())
}

fn validate_epoch(value: &Value) -> Result<EpochRecord, String> {
    let mut raw = value.clone();
    let object = raw.as_object_mut().ok_or("cache epoch is not an object")?;
    for key in ["hit_rate", "armed_hit_rate", "disarmed_hit_rate"] {
        object
            .remove(key)
            .ok_or(format!("cache epoch has no {key}"))?;
    }
    let record: EpochRecord =
        serde_json::from_value(raw).map_err(|e| format!("cache epoch: {e}"))?;
    for (key, rate) in [
        ("hit_rate", record.hit_rate()),
        ("armed_hit_rate", record.armed_hit_rate()),
        ("disarmed_hit_rate", record.disarmed_hit_rate()),
    ] {
        if value[key] != json!(rate) {
            return Err(format!("cache epoch {key} does not reconcile"));
        }
    }
    Ok(record)
}

/// Checked subtotals, never a price/cost estimate. Any unavailable scenario leaves
/// the all-scenario total unknown rather than contributing a fictitious zero.
pub fn totals(reports: &[Value]) -> Result<Value, String> {
    let mut totals = serde_json::Map::new();
    let incomplete = reports.iter().any(|r| r["class"] != "measured");
    for key in [
        "calls",
        "measured_calls",
        "input_tokens",
        "cached_tokens",
        "armed_input_tokens",
        "armed_cached_tokens",
        "disarmed_input_tokens",
        "disarmed_cached_tokens",
        "invalidations",
        "bytes_invalidated",
        "bytes_reclaimed",
        "tokens_invalidated",
        "tokens_reclaimed",
        "head_requests",
    ] {
        let mut sum = 0u64;
        let mut known = !incomplete;
        for report in reports.iter().filter(|r| r["class"] == "measured") {
            for epoch in report["epochs"]
                .as_array()
                .ok_or("cache totals require epochs")?
            {
                match epoch[key].as_u64() {
                    Some(n) => {
                        sum = sum
                            .checked_add(n)
                            .ok_or(format!("cache {key} subtotal overflow"))?
                    }
                    None => known = false,
                }
            }
        }
        totals.insert(key.into(), json!(known.then_some(sum)));
    }
    Ok(
        json!({"scope":"measured epoch subtotals; not economic cost", "all_scenarios_available":!incomplete,
        "values":totals}),
    )
}
