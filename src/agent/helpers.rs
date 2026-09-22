use super::{AgentError, CodingAgent};
use crate::adapter::ToolCall;
use crate::session::{ReservedRequest, RoundRecord, SessionStore, ToolCallRecord};

pub(super) fn tool_call_record(call: &ToolCall, ok: bool, result: String) -> ToolCallRecord {
    ToolCallRecord {
        id: call.id.clone(),
        name: call.name.clone(),
        args: call.args_json.clone(),
        ok,
        refused: false,
        result,
    }
}

/// A budget refusal: persisted for transcript fidelity, never counted
/// as an executed tool call.
pub(super) fn refused_call_record(call: &ToolCall, result: String) -> ToolCallRecord {
    ToolCallRecord {
        id: call.id.clone(),
        name: call.name.clone(),
        args: call.args_json.clone(),
        ok: false,
        refused: true,
        result,
    }
}

pub(super) fn final_summary_request() -> serdes_ai::core::ModelRequest {
    let mut request = serdes_ai::core::ModelRequest::new();
    request.add_user_prompt("Provide your final plain-text summary now (no tool calls).");
    request
}

pub(super) fn validate_provider_result(
    result: &super::LlmResult,
    secrets: &[String],
) -> Result<(), String> {
    let mut mapped_bytes = result.text.len();
    if result.text.len() > super::MAX_RESPONSE_BYTES {
        return Err(format!(
            "model response is {} bytes, over the {} byte cap",
            result.text.len(),
            super::MAX_RESPONSE_BYTES
        ));
    }
    if contains_secret(&result.text, secrets) {
        return Err("model response contained a configured secret".to_string());
    }
    for call in &result.calls {
        validate_provider_call(call, secrets)?;
        mapped_bytes = mapped_bytes
            .checked_add(call.id.len())
            .and_then(|total| total.checked_add(call.name.len()))
            .and_then(|total| total.checked_add(call.args_json.len()))
            .ok_or_else(|| "model response mapped size overflow".to_string())?;
        if mapped_bytes > super::MAX_RESPONSE_BYTES {
            return Err(format!(
                "mapped model response exceeds the {} byte cap",
                super::MAX_RESPONSE_BYTES
            ));
        }
    }
    Ok(())
}

fn validate_provider_call(call: &ToolCall, secrets: &[String]) -> Result<(), String> {
    if call.id.len() > super::MAX_TOOL_CALL_ID_BYTES {
        return Err(format!(
            "tool call id exceeds the {} byte cap",
            super::MAX_TOOL_CALL_ID_BYTES
        ));
    }
    if call.name.len() > super::MAX_TOOL_NAME_BYTES {
        return Err(format!(
            "tool name exceeds the {} byte cap",
            super::MAX_TOOL_NAME_BYTES
        ));
    }
    if [&call.id, &call.name, &call.args_json]
        .into_iter()
        .any(|value| contains_secret(value, secrets))
    {
        return Err("model response contained a configured secret".to_string());
    }
    Ok(())
}

fn contains_secret(value: &str, secrets: &[String]) -> bool {
    secrets
        .iter()
        .any(|secret| !secret.is_empty() && value.contains(secret))
}

impl super::CodingAgent {
    pub fn workspace_cap(&self) -> &crate::tools::WorkspaceCap {
        &self.workspace
    }
}

/// How the turn tells the model what its tool-call budget looks like: the
/// plain remaining count, a wrap-up nudge near the end, and a final-replies-only
/// notice once nothing is left. An unlimited budget stays silent.
pub(crate) fn budget_notice(budget: Option<usize>, used: usize) -> String {
    let Some(max) = budget else {
        return String::new();
    };
    let left = max.saturating_sub(used);
    if left == 0 {
        format!("[budget: 0 of {max} tool calls left; reply with your final summary only]")
    } else if left <= 3 {
        format!("[budget: only {left} of {max} tool calls left; wrap up and produce your final summary]")
    } else {
        format!("[budget: {left} of {max} tool calls left]")
    }
}

/// Split off the tool calls that no longer fit the budget. Returns the
/// refused tail; `fit` keeps only the executable prefix.
pub(super) fn split_over_budget(
    budget: Option<usize>,
    usage: &super::TurnUsage,
    fit: &mut Vec<crate::adapter::ToolCall>,
) -> Vec<crate::adapter::ToolCall> {
    let Some(max) = budget else {
        return Vec::new();
    };
    let allowed = max.saturating_sub(usage.total_calls);
    if allowed >= fit.len() {
        return Vec::new();
    }
    fit.split_off(allowed)
}

/// Record a refusal for every unknown or disabled call so the model receives
/// a corrective tool result and can continue the turn.
pub(super) fn refuse_unknown_tools(
    allow_shell: bool,
    attempt: &mut super::AttemptState,
    round: &mut super::RoundRecord,
    refused: &[ToolCall],
) {
    let roster = crate::tools::tool_specs(allow_shell)
        .into_iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>()
        .join(", ");
    for call in refused {
        let text = format!(
            "error: unknown or disabled tool {}; available: {roster}",
            call.name
        );
        // The errored call still consumes a budget slot and is recorded in history like
        // every other call (the restore path derives `total_calls` from all round calls),
        // so a model that keeps hallucinating names cannot spin forever for free.
        attempt.usage.total_calls += 1;
        attempt.usage.output_bytes = attempt.usage.output_bytes.saturating_add(text.len());
        attempt.requests.push(super::tool_return_request(
            &call.name, &call.id, false, &text,
        ));
        round.calls.push(refused_call_record(call, text));
    }
}

/// Record a refusal for every over-budget call so the assistant's tool
/// message stays protocol-valid and the model learns why nothing ran.
pub(super) fn refuse_over_budget(
    budget: Option<usize>,
    attempt: &mut super::AttemptState,
    round: &mut super::RoundRecord,
    skipped: &[crate::adapter::ToolCall],
) {
    for call in skipped {
        let notice = budget_notice(budget, attempt.usage.total_calls);
        let text = format!("tool call refused: tool-call budget exhausted\n\n{notice}");
        attempt.requests.push(super::tool_return_request(
            &call.name, &call.id, false, &text,
        ));
        round.calls.push(refused_call_record(call, text));
    }
}

impl CodingAgent {
    /// Sets the digest admission floor (issue 125) the session resolved.
    ///
    /// Validated exactly like the settings layer: a floor below the version-1 baseline
    /// would tighten the filter registry (refused in-session) and break `safe_window`'s
    /// strictly-under invariant, so it is a typed `Config` error here rather than a value
    /// that silently never digests.
    pub fn with_digest_size_floor(
        mut self,
        floor: usize,
    ) -> Result<CodingAgent, crate::agent::AgentError> {
        if floor < crate::context_ingress::filter::DEFAULT_DIGEST_SIZE_FLOOR {
            return Err(crate::agent::AgentError::new(
                crate::envelope::Code::Config,
                "digest-size-floor",
                format!(
                    "digest-size-floor ({floor}) must be at least the baseline floor ({})",
                    crate::context_ingress::filter::DEFAULT_DIGEST_SIZE_FLOOR
                ),
            ));
        }
        self.digest_size_floor = floor;
        Ok(self)
    }
}

impl super::CodingAgent {
    /// Persist a terminal failure and surface it as an [`AgentError`]. The message is
    /// scrubbed first (every accepted secret and credential path), then bounded to
    /// [`crate::redact::MAX_ERROR_TEXT_BYTES`] at a UTF-8 boundary with the
    /// explicit `[truncated]` marker; that bounded scrubbed text is what the
    /// session fail and the CLI JSON both receive, so a huge provider body must leave a
    /// terminal failed lifecycle and retain the model exit code, never become
    /// session-persist. A persistence failure is never discarded: it becomes a
    /// session error that still carries the original scrubbed bounded message.
    pub(super) fn dead(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        key: &'static str,
        message: &str,
        rounds: &[RoundRecord],
    ) -> AgentError {
        self.dead_coded(
            store,
            reserved,
            crate::envelope::Code::Model,
            key,
            message,
            rounds,
        )
    }

    /// [`Self::dead`] for failures that already carry their own classification:
    /// the terminal record keeps the failing error's code and key instead of
    /// reclassifying every setup failure as a model failure.
    pub(super) fn dead_coded(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        code: crate::envelope::Code,
        key: &'static str,
        message: &str,
        rounds: &[RoundRecord],
    ) -> AgentError {
        let bounded = crate::redact::scrub_and_bound(message, &self.secrets);
        match store.fail(reserved, &bounded, rounds) {
            Ok(()) => {
                let profile = self.profile_store(store, "session_written", rounds.len(), None);
                match profile {
                    Ok(()) => AgentError::new(code, key, bounded),
                    Err(profile_error) => profile_error,
                }
            }
            Err(pe) => AgentError::new(
                crate::envelope::Code::Session,
                "session-persist",
                format!(
                    "turn failed ({key}: {bounded}); additionally, persisting the failure failed: {pe}"
                ),
            ),
        }
    }
}
