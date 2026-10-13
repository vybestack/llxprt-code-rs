//! Durable outgoing-request prefix authority. Payloads stay encrypted in the
//! existing vault, not in policy telemetry or plaintext transcript artifacts.
use crate::context_policy::cache::{CheckpointCut, CutTrigger, EpochError};
use crate::session::{ReservedRequest, SessionStore, StoreError};
use serde::{Deserialize, Serialize};
use serdes_ai::core::ModelRequest;
use sha2::{Digest, Sha256};

pub(crate) type RequestPrefixes = std::collections::BTreeMap<String, RequestPrefix>;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RequestPrefix {
    pub epoch: u64,
    pub handle: String,
    pub digest: String,
    pub requests: usize,
}

fn encode(requests: &[ModelRequest]) -> Result<Vec<u8>, StoreError> {
    serde_json::to_vec(requests)
        .map_err(|e| StoreError::Invalid(format!("encode request prefix: {e}")))
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn load(
    state: &super::context_persist::ContextState,
    prefix: &RequestPrefix,
) -> Result<Vec<ModelRequest>, StoreError> {
    let bytes = state
        .store
        .vault_get(&prefix.handle)
        .map_err(|_| StoreError::Corrupt("context request prefix vault refused read".into()))?;
    if digest(&bytes) != prefix.digest {
        return Err(StoreError::Corrupt(
            "context request prefix digest mismatch".into(),
        ));
    }
    let requests: Vec<ModelRequest> = serde_json::from_slice(&bytes)
        .map_err(|e| StoreError::Corrupt(format!("context request prefix corrupt: {e}")))?;
    if requests.len() != prefix.requests {
        return Err(StoreError::Corrupt(
            "context request prefix length mismatch".into(),
        ));
    }
    Ok(requests)
}

impl SessionStore {
    /// Preserve the exact prior-turn wire projection on restart. A new turn
    /// appends the final response and its new prompt to the previous sent prefix.
    pub(crate) fn resume_request_prefix(
        &self,
        reserved: &ReservedRequest,
        fallback: Vec<ModelRequest>,
        tail: Vec<ModelRequest>,
    ) -> Result<Vec<ModelRequest>, StoreError> {
        let mut guard = self
            .context
            .lock()
            .map_err(|_| StoreError::Lock("context mutex poisoned".into()))?;
        if guard.is_none() {
            *guard = Some(
                super::context_persist::recover_context_state(self).map_err(StoreError::Invalid)?,
            );
        }
        let state = guard
            .as_mut()
            .ok_or_else(|| StoreError::Lock("context state missing".into()))?;
        let Some(parent) = reserved.history.last() else {
            return Ok(fallback);
        };
        let Some(prefix) = state.request_prefixes.get(&parent.branch_id).cloned() else {
            // Store-only turns (no provider request ever sent) have no prefix.
            if state.policy.epoch_cache().records().is_empty() {
                return Ok(fallback);
            }
            return Err(StoreError::Corrupt(
                "completed request prefix is missing".into(),
            ));
        };
        let mut requests = load(state, &prefix)?;
        requests.extend(tail);
        state
            .request_prefixes
            .insert(reserved.branch_id.clone(), prefix);
        Ok(requests)
    }

    /// Save before send, including failed sends: later retries must either
    /// append to these exact bytes or commit an explicit checkpoint first.
    pub(crate) fn record_request_prefix(
        &self,
        reserved: &ReservedRequest,
        requests: &[ModelRequest],
        trigger: Option<CutTrigger>,
    ) -> Result<u64, StoreError> {
        let mut guard = self
            .context
            .lock()
            .map_err(|_| StoreError::Lock("context mutex poisoned".into()))?;
        if guard.is_none() {
            *guard = Some(
                super::context_persist::recover_context_state(self).map_err(StoreError::Invalid)?,
            );
        }
        let state = guard
            .as_mut()
            .ok_or_else(|| StoreError::Lock("context state missing".into()))?;
        let prior = state.request_prefixes.get(&reserved.branch_id).cloned();
        let old = prior
            .as_ref()
            .map(|prefix| load(state, prefix))
            .transpose()?;
        let append = old.as_ref().is_none_or(|old| requests.starts_with(old));
        if !append && trigger.is_none() {
            return Err(StoreError::Invalid(EpochError::PrefixChanged.to_string()));
        }
        if let Some(trigger) = trigger {
            if !state.policy.checkpoint_allowed(trigger) {
                return Err(StoreError::Invalid(EpochError::GovernorRefused.to_string()));
            }
        }
        let bytes = encode(requests)?;
        let head_digest = digest(&bytes);
        let epoch = if let Some(trigger) = trigger {
            let old_bytes = old
                .as_ref()
                .map(|old| encode(old))
                .transpose()?
                .map_or(0, |b| b.len() as u64);
            state
                .policy
                .checkpoint(CheckpointCut {
                    head_digest: head_digest.clone(),
                    head_requests: requests.len(),
                    trigger,
                    armed: trigger == CutTrigger::BudgetPressure,
                    bytes_reclaimed: old_bytes.saturating_sub(bytes.len() as u64),
                    bytes_invalidated: old_bytes,
                    tokens_invalidated: None,
                })
                .map_err(|e| StoreError::Invalid(e.to_string()))?
        } else if let Some(prior) = &prior {
            prior.epoch
        } else {
            state
                .policy
                .epoch_cache_mut()
                .start(head_digest.clone(), requests.len())
                .map_err(|e| StoreError::Invalid(e.to_string()))?
        };
        let handle = state
            .store
            .vault_put(&bytes, "request-prefix")
            .map_err(|_| {
                StoreError::Invalid("context request prefix vault refused write".into())
            })?;
        retire(state, &reserved.branch_id, prior.as_ref())?;
        // Keep parent handles: branches can legitimately fork from that prefix.
        state.request_prefixes.insert(
            reserved.branch_id.clone(),
            RequestPrefix {
                epoch,
                handle,
                digest: head_digest,
                requests: requests.len(),
            },
        );
        super::context_publish::persist_context(self, state).map_err(StoreError::Invalid)?;
        Ok(epoch)
    }

    pub(crate) fn observe_epoch_usage(
        &self,
        epoch: u64,
        input: Option<u64>,
        cached: Option<u64>,
        pressure_cut: bool,
    ) -> Result<(), StoreError> {
        let mut guard = self
            .context
            .lock()
            .map_err(|_| StoreError::Lock("context mutex poisoned".into()))?;
        let state = guard
            .as_mut()
            .ok_or_else(|| StoreError::Lock("context state missing".into()))?;
        let armed = pressure_cut || state.policy.armed();
        state
            .policy
            .epoch_cache_mut()
            .observe(epoch, input, cached, armed)
            .map_err(|e| StoreError::Invalid(e.to_string()))?;
        super::context_publish::persist_context(self, state).map_err(StoreError::Invalid)
    }
}

fn retire(
    state: &mut super::context_persist::ContextState,
    branch: &str,
    prior: Option<&RequestPrefix>,
) -> Result<(), StoreError> {
    if let Some(old) = prior {
        if !state
            .request_prefixes
            .iter()
            .any(|(other, prefix)| other != branch && prefix.handle == old.handle)
        {
            state
                .store
                .vault_erase(&old.handle)
                .map_err(|_| StoreError::Invalid("context prefix retirement failed".into()))?;
        }
    }
    Ok(())
}

/// Check recovered metadata after restoring the authenticated vault. Unknown
/// epochs and missing/tampered payloads cannot quietly mint a fresh prefix.
pub(crate) fn validate(state: &super::context_persist::ContextState) -> Result<(), String> {
    for prefix in state.request_prefixes.values() {
        if !state
            .policy
            .epoch_cache()
            .records()
            .iter()
            .any(|r| r.epoch == prefix.epoch)
        {
            return Err("context request prefix names an unknown epoch".into());
        }
        load(state, prefix).map_err(|e| e.to_string())?;
    }
    Ok(())
}
