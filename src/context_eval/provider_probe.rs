//! Independent crash protocol inspection. An unresolved durable intent is a
//! successful safety outcome, never successful task output or license to resend.
use crate::context_gateway::Status;
use crate::session::{SessionId, SessionStore};
use std::path::Path;

pub(super) fn safe_unresolved(home: &Path, session: &str) -> bool {
    let Ok(id) = SessionId::parse(session) else {
        return false;
    };
    let Ok(store) = SessionStore::load_at(&id, home) else {
        return false;
    };
    let Ok(state) = store.snapshot() else {
        return false;
    };
    !state.provider_turns.is_empty()
        && state.provider_turns.iter().all(|r| {
            matches!(
                r.status,
                Status::Intent | Status::Pending | Status::Indeterminate
            ) && r.executions.is_empty()
        })
}

use sha2::Digest;
use std::fs;
pub(super) fn manifest_file_digest(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    Ok(super::hex_digest(&sha2::Sha256::digest(&bytes)))
}

/// Deterministic lease-expiry fault fixture; no production timeout is changed.
pub(super) fn expire(prepared: &super::runner::Prepared) -> Result<(), String> {
    let id = SessionId::parse(&prepared.session).map_err(|e| e.to_string())?;
    let store = SessionStore::load_at(&id, &prepared.config_home).map_err(|e| e.to_string())?;
    let mut state = store.snapshot().map_err(|e| e.to_string())?;
    for branch in &mut state.branches {
        if branch.lifecycle == crate::session::Lifecycle::Pending {
            branch.reserved_at = 1;
            branch.lease_expiry = 2;
        }
    }
    store.replace_snapshot(&state).map_err(|e| e.to_string())
}
pub(super) fn indeterminate(result: &crate::harness::BbResult) -> bool {
    result.status == "error"
        && result.exit.is_some_and(|e| e != 0)
        && serde_json::from_slice::<serde_json::Value>(&result.raw_stdout)
            .ok()
            .and_then(|v| {
                v["error"]["message"]
                    .as_str()
                    .map(|m| m.contains("indeterminate"))
            })
            .unwrap_or(false)
}

use super::{
    harness, inject, runner, save_turn_artifacts, session_dir, BbResult, Evidence, InvocationSpec,
    Scenario,
};
/// Recovery probe after an executed fault: the killed process is gone, so the next
/// invocation is a real process restart against the same session. A valid envelope of
/// either kind (ok, or a clean typed error such as a context refusal) proves the
/// persisted store reopened and replayed; no envelope means the restart path is broken.
pub(super) fn recovery_probe(
    scen: &Scenario,
    prepared: &runner::Prepared,
    out_dir: &Path,
    evidence: &mut Evidence,
    results: &[BbResult],
) -> Result<(), String> {
    let probe_index = results.len();
    if scen.id == "provider-crash-matrix" {
        expire(prepared)?;
    }
    let probe = InvocationSpec {
        session: prepared.session.clone(),
        cwd: prepared.workspace.clone(),
        prompt: if scen.id == "provider-crash-matrix" {
            scen.stimulus.prompt.clone()
        } else {
            "CTXEVAL-RESTART-PROBE: confirm this session reopened after the restart; reply briefly."
                .into()
        },
        turn: if scen.id == "provider-crash-matrix" {
            None
        } else {
            Some(probe_index as u32 + 1)
        },
        branch: None,
        profile: Some(prepared.profile_name.clone()),
        allow_insecure_http: true,
        allow_shell: false,
    };
    let result = harness::run_cli(probe);
    save_turn_artifacts(out_dir, probe_index, &result)?;
    let restarted = if scen.id == "provider-crash-matrix" {
        indeterminate(&result)
    } else {
        result.status == "ok" || result.status == "error"
    };
    evidence.recovery_after_fault = restarted
        && session_dir(prepared)
            .map(|session| inject::store_shape_consistent(&session.join("context")))
            .unwrap_or(false);
    let probe_outcome = if restarted {
        "session reopened with a valid envelope"
    } else {
        "no valid envelope (status mismatch)"
    };
    harness::eprint_status(&format!(
        "context-evals recovery probe after {}: {probe_outcome}",
        evidence.faults_executed.join(", ")
    ));
    Ok(())
}
