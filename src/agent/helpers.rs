use crate::adapter::ToolCall;
use crate::session::ToolCallRecord;

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
