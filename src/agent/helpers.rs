use crate::adapter::ToolCall;
use crate::session::ToolCallRecord;

pub(super) fn tool_call_record(call: &ToolCall, ok: bool, result: String) -> ToolCallRecord {
    ToolCallRecord {
        id: call.id.clone(),
        name: call.name.clone(),
        args: call.args_json.clone(),
        ok,
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
