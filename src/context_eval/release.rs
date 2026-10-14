//! Complete, fail-closed release inventory. Unknown/live claims remain unavailable;
//! expected-red screening is not silently promoted to release qualification.
use super::{endurance::Endurance, report};
use crate::context_txn::operation;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

pub const PROTOCOL: &str = include_str!("endurance-protocol.md");
pub const ARMS: &[(&str, &str)] = &[
    ("status-quo", "full selected-lineage replay"),
    (
        "minimum-floor",
        "quotas, handles, store, deterministic range read-back",
    ),
    (
        "entry-filter",
        "frozen / relaxation-only / post-entry; labeled-span recall",
    ),
    ("lane-blind-gate", "lane-typed / lane-blind gate"),
    ("registry", "lane policy / uniform fidelity"),
    ("ledger-admission", "ledger admission on / off"),
    ("ledger-decay", "ledger decay on / off"),
    ("validation", "typed validation on / off"),
    (
        "rooted-support",
        "rooted / flat support; >=5 recurrent rounds",
    ),
    ("read-back", "range read-back on / off"),
    ("branching", "read-only branches on / off"),
    ("triggers", "predictive / threshold reactive"),
    ("estimator", "estimated / fixed within-rung ordering"),
    ("ladder-order", "selection-first / generation-first"),
    ("notes-placement", "near-head / after-body"),
    (
        "source-isolation",
        "shared evidential transcript / decision projection",
    ),
    ("capability-tier", "per-tier lane fidelity profiles"),
    (
        "provider-native",
        "provider-native context-management baseline",
    ),
    ("ingress-rate", "high-rate/high-volume quota-floor stress"),
    (
        "security",
        "management/task injection, leak, scorer/index/estimator poisoning, branch/delegation",
    ),
];
pub const CHECKPOINTS: &[(&str, &str)] = &[
    ("closure", "executor commit / shadow log replay"),
    ("authority", "executor receipt / authenticated ingress"),
    ("net-reclaim", "validated reclamation potential"),
    ("occupancy-protection", "universal fit and protection floor"),
    ("forward-progress", "guarded policy/executor macrostep"),
    (
        "rematerialization",
        "render request hash and restart interval",
    ),
    ("pairing", "commit and send legality"),
    ("provenance", "leaf-rooted bidirectional validation"),
    (
        "governor",
        "admitted <= alpha * measured reclamation, floor then quiesce",
    ),
];

pub fn protocol_digest() -> String {
    super::hex_digest(&Sha256::digest(PROTOCOL.as_bytes()))
}
fn unavailable(reason: &str) -> Value {
    json!({"status":"unavailable", "reason":reason})
}

pub fn assemble(screen: &Value, cli: &Path, endurance: Option<&Path>) -> Result<Value, String> {
    report::validate(screen, true)?;
    let head = screen["runner_revision"]
        .as_str()
        .ok_or("screen has no revision")?;
    let binary = std::fs::read(cli).map_err(|e| format!("read CLI for release digest: {e}"))?;
    let endurance = match endurance {
        Some(path) => {
            let bytes = std::fs::read(path).map_err(|e| format!("read endurance evidence: {e}"))?;
            let evidence: Endurance =
                serde_json::from_slice(&bytes).map_err(|e| format!("endurance evidence: {e}"))?;
            evidence.validate(head, &protocol_digest())?;
            json!({"status":"validated-structure", "sha256":super::hex_digest(&Sha256::digest(&bytes)),
                "evidence":evidence, "limitation":"operator must independently attest raw observations"})
        }
        None => unavailable(
            "48 recorded hours with >=3 injected restarts and matched control not supplied",
        ),
    };
    let operations: Vec<Value> = operation::registry()
        .iter()
        .map(|row| {
            json!({
                "name":row.name,"owner_phase":row.owner_phase,"proposer":row.proposer.as_str(),
                "reclamation":row.reclamation,"bar":row.bar,"design":"section 9 / tab:ops",
            })
        })
        .collect();
    let arms: Vec<Value> = ARMS.iter().map(|(id,comparison)| json!({"id":id,"comparison":comparison,
        "design":"section 21 / sec:eval", "evidence":unavailable("mechanism-specific matched comparative run not supplied")
    })).collect();
    let checkpoints: Vec<Value> = CHECKPOINTS.iter().map(|(id,boundary)| json!({
        "id":id,"boundary":boundary,"design":"section 18 / tab:invariants",
        "evidence":unavailable("composed live checkpoint product not attested by this screen"),
    })).collect();
    let architecture_acceptance = super::acceptance::assemble(screen)?;
    let result = json!({
        "schema_version":2,"tracker_issue":46,"candidate_sha":head,
        "binary_sha256":super::hex_digest(&Sha256::digest(&binary)),"protocol_digest":protocol_digest(),
        "claim_status":"deterministic-screen-only; not architecture acceptance",
        "release_qualified":false,
        "review_cycle_result":"skipped per Andrew directive 2026-10-09 (no OCR)",
        "architecture_acceptance":architecture_acceptance,
        "screen":screen,"operations":operations,"checkpoints":checkpoints,"arms":arms,
        "thresholds": {"out_of_branch_wall_hits":0,"unquiesced_armed_noops":0,
            "duration_ms":super::endurance::DURATION_FLOOR_MS,"minimum_restarts":super::endurance::MIN_RESTARTS,
            "non_inferiority_margin_bp":super::endurance::NON_INFERIORITY_MARGIN_BP,"bounded_continuation_floor_bp":9900},
        "endurance":endurance,
        "economic_claims":unavailable("unpriced telemetry and unmeasured comparisons excluded; no zero-cost substitution"),
        "offline_qualification":unavailable("paired held-out closed-loop continuation corpus not supplied"),
        "benchmark_pins":unavailable("external dataset revisions and content digests not supplied; see protocol"),
    });
    validate(&result)?;
    Ok(result)
}

pub fn validate(value: &Value) -> Result<(), String> {
    for field in [
        "schema_version",
        "tracker_issue",
        "candidate_sha",
        "binary_sha256",
        "protocol_digest",
        "claim_status",
        "release_qualified",
        "review_cycle_result",
        "screen",
        "architecture_acceptance",
        "operations",
        "checkpoints",
        "arms",
        "thresholds",
        "endurance",
        "economic_claims",
        "offline_qualification",
        "benchmark_pins",
    ] {
        if value.get(field).is_none() {
            return Err(format!("release report missing {field}"));
        }
    }
    if value["schema_version"] != 2
        || value["tracker_issue"] != 46
        || !super::endurance::digest(value["candidate_sha"].as_str().unwrap_or(""), 40)
        || !super::endurance::digest(value["binary_sha256"].as_str().unwrap_or(""), 64)
        || value["protocol_digest"] != protocol_digest()
        || value["release_qualified"] != false
    {
        return Err("release identity mismatch or unsupported qualification claim".into());
    }
    if value["review_cycle_result"] != "skipped per Andrew directive 2026-10-09 (no OCR)"
        || value["claim_status"] != "deterministic-screen-only; not architecture acceptance"
    {
        return Err("release claim/review attribution changed".into());
    }
    report::validate(&value["screen"], true)?;
    if value["candidate_sha"] != value["screen"]["runner_revision"] {
        return Err("release and screen revisions differ".into());
    }
    if value["architecture_acceptance"] != super::acceptance::assemble(&value["screen"])? {
        return Err("architecture acceptance does not reconcile to screen".into());
    }
    inventory(value, "arms", "id", ARMS.iter().map(|(id, _)| *id))?;
    inventory(
        value,
        "checkpoints",
        "id",
        CHECKPOINTS.iter().map(|(id, _)| *id),
    )?;
    inventory(
        value,
        "operations",
        "name",
        operation::registry().iter().map(|r| r.name),
    )?;
    let expected = json!({"out_of_branch_wall_hits":0,"unquiesced_armed_noops":0,
        "duration_ms":super::endurance::DURATION_FLOOR_MS,"minimum_restarts":super::endurance::MIN_RESTARTS,
        "non_inferiority_margin_bp":super::endurance::NON_INFERIORITY_MARGIN_BP,"bounded_continuation_floor_bp":9900});
    if value["thresholds"] != expected {
        return Err("release thresholds differ from predeclared protocol".into());
    }
    validate_metadata(value)
}
fn validate_metadata(value: &Value) -> Result<(), String> {
    for field in ["economic_claims", "offline_qualification", "benchmark_pins"] {
        validate_unavailable(&value[field])?;
    }
    for row in value["arms"].as_array().ok_or("release arms missing")? {
        let (_, comparison) = ARMS
            .iter()
            .find(|(id, _)| row["id"] == *id)
            .ok_or("unknown arm")?;
        if row["comparison"] != *comparison {
            return Err("arm comparison changed".into());
        }
        validate_unavailable(&row["evidence"])?;
    }
    for row in value["checkpoints"]
        .as_array()
        .ok_or("release checkpoints missing")?
    {
        let (_, boundary) = CHECKPOINTS
            .iter()
            .find(|(id, _)| row["id"] == *id)
            .ok_or("unknown checkpoint")?;
        if row["boundary"] != *boundary {
            return Err("checkpoint boundary changed".into());
        }
        validate_unavailable(&row["evidence"])?;
    }
    for row in value["operations"]
        .as_array()
        .ok_or("release operations missing")?
    {
        let op = operation::find(row["name"].as_str().ok_or("missing operation name")?)
            .ok_or("unknown operation")?;
        if row["owner_phase"] != op.owner_phase
            || row["proposer"] != op.proposer.as_str()
            || row["reclamation"] != op.reclamation
            || row["bar"] != op.bar
        {
            return Err("release operation metadata disagrees with registry".into());
        }
    }
    if value["endurance"]["status"] == "unavailable" {
        validate_unavailable(&value["endurance"])?;
    } else {
        let evidence: Endurance = serde_json::from_value(value["endurance"]["evidence"].clone())
            .map_err(|e| format!("release endurance: {e}"))?;
        evidence.validate(
            value["candidate_sha"].as_str().ok_or("missing head")?,
            &protocol_digest(),
        )?;
        if value["endurance"]["status"] != "validated-structure"
            || !super::endurance::digest(value["endurance"]["sha256"].as_str().unwrap_or(""), 64)
        {
            return Err("release endurance identity invalid".into());
        }
    }
    Ok(())
}
fn validate_unavailable(value: &Value) -> Result<(), String> {
    if value["status"] != "unavailable" || value["reason"].as_str().is_none_or(str::is_empty) {
        return Err("release unavailable field lacks explicit reason".into());
    }
    Ok(())
}
fn inventory<'a>(
    value: &Value,
    field: &str,
    key: &str,
    expected: impl Iterator<Item = &'a str>,
) -> Result<(), String> {
    let rows = value[field]
        .as_array()
        .ok_or(format!("release {field} is not an array"))?;
    let mut actual = std::collections::BTreeSet::new();
    for row in rows {
        let name = row[key]
            .as_str()
            .ok_or(format!("release {field} missing {key}"))?;
        if !actual.insert(name) || row["design"].as_str().is_none_or(str::is_empty) {
            return Err(format!(
                "release {field} duplicate or missing design mapping"
            ));
        }
    }
    if actual != expected.collect() {
        return Err(format!("release {field} inventory is incomplete"));
    }
    Ok(())
}
