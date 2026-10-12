//! Calibration in the existing request-budget path, separate from host byte admission.
use crate::telemetry::bounds::{BoundTerms, RequestBound, RequestContract};
use sha2::{Digest, Sha256};
mod contract;

/// Code/data identity is mandatory; no model tokenizer substitution is allowed.
pub trait Tokenizer {
    fn identity(&self) -> (&str, &str);
    fn count(&self, wire: &[u8]) -> Result<u64, CalibrationError>;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CalibrationError {
    #[error("request calibration arithmetic overflow")]
    Overflow,
    #[error("held-out request exceeds calibrated margin")]
    Underbound,
    #[error("request tokenizer contract mismatch")]
    TokenizerMismatch,
    #[error("request wire projection is not valid JSON")]
    Projection,
    #[error("request calibration capability is unavailable")]
    Unavailable,
}

/// Only complete observations from the identical contract contribute. Cached/reasoning
/// counters are not added: input is normalized at the provider boundary.
pub fn margin(contract: &RequestContract, observations: &[RequestBound]) -> Option<u64> {
    observations
        .iter()
        .filter(|b| &b.contract == contract)
        .filter_map(|b| b.estimated_input.zip(b.observed_input))
        .map(|(estimate, observed)| observed.saturating_sub(estimate))
        .max()
}

pub fn check_held_out(bound: &RequestBound) -> Result<(), CalibrationError> {
    let (Some(input), Some(observed)) = (bound.bounded_input, bound.observed_input) else {
        return Err(CalibrationError::Unavailable);
    };
    if observed > input {
        Err(CalibrationError::Underbound)
    } else {
        Ok(())
    }
}

/// Measure the selected projection, not abstract history. The tokenizer-unavailable
/// policy retains the existing explicitly labelled byte heuristic, never a tokenizer claim.
pub fn measure(
    contract: RequestContract,
    wire: &[u8],
    tokenizer: Option<&dyn Tokenizer>,
    context_limit: Option<u64>,
    observations: &[RequestBound],
) -> Result<RequestBound, CalibrationError> {
    verify_tokenizer(&contract, tokenizer)?;
    let contract = contract::selected(contract, wire)?;
    let terms = terms(wire, tokenizer)?;
    let estimated_input = Some(count(wire, tokenizer)?);
    let margin = margin(&contract, observations);
    let bounded_input = estimated_input
        .zip(margin)
        .map(|(input, margin)| input.checked_add(margin).ok_or(CalibrationError::Overflow))
        .transpose()?;
    Ok(RequestBound {
        contract,
        wire_digest: format!("{:x}", Sha256::digest(wire)),
        wire_bytes: wire.len() as u64,
        terms,
        estimated_input,
        observed_input: None,
        margin,
        bounded_input,
        context_limit,
        host_memory_cap_bytes: super::materialization_budget(context_limit) as u64,
        capability: if tokenizer.is_some() {
            "tokenizer_available"
        } else {
            "tokenizer_unavailable_byte_heuristic"
        }
        .into(),
    })
}

fn verify_tokenizer(
    contract: &RequestContract,
    tokenizer: Option<&dyn Tokenizer>,
) -> Result<(), CalibrationError> {
    let identity = tokenizer.map(Tokenizer::identity);
    if contract.tokenizer.as_deref() != identity.map(|(name, _)| name)
        || contract.tokenizer_version.as_deref() != identity.map(|(_, version)| version)
    {
        return Err(CalibrationError::TokenizerMismatch);
    }
    Ok(())
}

fn count(wire: &[u8], tokenizer: Option<&dyn Tokenizer>) -> Result<u64, CalibrationError> {
    match tokenizer {
        Some(tokenizer) => tokenizer.count(wire),
        None => Ok((wire.len() as u64).div_ceil(3)),
    }
}

fn terms(wire: &[u8], tokenizer: Option<&dyn Tokenizer>) -> Result<BoundTerms, CalibrationError> {
    let mut value: serde_json::Value =
        serde_json::from_slice(wire).map_err(|_| CalibrationError::Projection)?;
    if let Some(response) = value.get("response") {
        value = response.clone();
    }
    let count = |value: &serde_json::Value| -> Result<Option<u64>, CalibrationError> {
        let bytes = serde_json::to_vec(value).map_err(|_| CalibrationError::Projection)?;
        Ok(Some(count(&bytes, tokenizer)?))
    };
    let tools = value
        .get("tools")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let mut system = vec![];
    for key in ["system", "instructions"] {
        if let Some(head) = value.get(key) {
            system.push(head.clone());
        }
    }
    let mut body = value
        .get("messages")
        .or_else(|| value.get("input"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    if let Some(messages) = body.as_array_mut() {
        messages.retain(|message| {
            if matches!(
                message.get("role").and_then(|v| v.as_str()),
                Some("system" | "developer")
            ) {
                system.push(message.clone());
                false
            } else {
                true
            }
        });
    }
    let output_reserve = ["max_output_tokens", "max_completion_tokens", "max_tokens"]
        .iter()
        .find_map(|key| value.get(key).and_then(|v| v.as_u64()));
    for key in ["tools", "system", "instructions", "messages", "input"] {
        if let Some(object) = value.as_object_mut() {
            object.remove(key);
        }
    }
    Ok(BoundTerms {
        system: count(&serde_json::Value::Array(system))?,
        tools: count(&tools)?,
        body: count(&body)?,
        framing: count(&value)?,
        output_reserve,
        terminal_reserve: 0,
    })
}

#[cfg(test)]
mod tests;
