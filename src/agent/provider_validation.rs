//! Provider reply admission precedes transcripts, usage and tool dispatch.
//! Names are provider function-name fields, not names extracted from assistant prose.
use super::{LlmResult, RoundFailure};

pub(super) fn validate(result: &LlmResult, secrets: &[String]) -> Result<(), RoundFailure> {
    let mapped_bytes = result
        .calls
        .iter()
        .try_fold(result.text.len(), |total, call| {
            total
                .checked_add(call.id.len())?
                .checked_add(call.name.len())?
                .checked_add(call.args_json.len())
        });
    if mapped_bytes.is_none_or(|bytes| bytes > super::MAX_RESPONSE_BYTES) {
        return Err(RoundFailure::Model(format!(
            "mapped model response exceeds the {} byte cap",
            super::MAX_RESPONSE_BYTES
        )));
    }
    // Check the complete reply before selecting any diagnostic or cutting a span.
    if std::iter::once(&result.text)
        .chain(
            result
                .calls
                .iter()
                .flat_map(|call| [&call.id, &call.name, &call.args_json]),
        )
        .any(|value| {
            secrets
                .iter()
                .any(|secret| !secret.is_empty() && value.contains(secret))
        })
    {
        return Err(RoundFailure::Model(
            "model response contained a configured secret".into(),
        ));
    }
    for call in &result.calls {
        if call.id.len() > super::MAX_TOOL_CALL_ID_BYTES {
            return Err(RoundFailure::Model(format!(
                "tool call id exceeds the {} byte cap",
                super::MAX_TOOL_CALL_ID_BYTES
            )));
        }
    }
    for (index, call) in result.calls.iter().enumerate() {
        let reason = if call.name.len() > super::MAX_TOOL_NAME_BYTES {
            Some(format!(
                "tool name exceeds the {} byte cap",
                super::MAX_TOOL_NAME_BYTES
            ))
        } else if !crate::tools::is_safe_tool_name(&call.name) {
            Some("model returned an invalid tool name identifier".into())
        } else {
            None
        };
        if let Some(reason) = reason {
            // Recovery must not hide malformed arguments/ids or a rejected finish.
            super::finish_check(result).map_err(RoundFailure::FinishReason)?;
            super::validate_calls(&mut std::collections::HashSet::new(), result)
                .map_err(RoundFailure::InvalidToolCall)?;
            return Err(RoundFailure::ToolCallRefusal(name_diagnostic(
                index, &call.name, &reason, secrets,
            )));
        }
    }
    Ok(())
}

fn name_diagnostic(index: usize, name: &str, reason: &str, secrets: &[String]) -> String {
    // Scrub the whole field before the lossy bound; JSON escaping makes controls inert.
    let scrubbed = crate::redact::scrub_secrets(name, secrets);
    let excerpt = crate::redact::truncate_utf8(scrubbed, 256);
    let quoted = serde_json::to_string(&excerpt).expect("string serialization");
    format!("{reason}; source=mapped provider tool_calls[{index}].name; rejected field bytes=0..{}; sanitized excerpt={quoted}", name.len())
}

impl super::Turn<'_> {
    /// A malformed name refuses the entire reply. One corrective reissue uses the
    /// same backend and clock, without replaying unsafe fields or executing its batch.
    pub(super) fn round(
        &self,
        requests: &[serdes_ai::core::ModelRequest],
        tools: &[crate::tools::ToolSpec],
    ) -> Result<LlmResult, RoundFailure> {
        let first = match self.round_once(requests, tools) {
            Err(RoundFailure::ToolCallRefusal(message)) => message,
            other => return other,
        };
        let mut correction = requests.to_vec();
        correction.push(crate::adapter::user_request(
            "Your previous tool-call reply was refused: a function name was invalid or exceeded \
             the 64-byte cap. None of that reply's tools ran. Use only the exact registered \
             tool names, or provide your final plain-text summary.",
        ));
        if super::round_budget_exceeded(&correction, tools, self.context_limit) {
            return Err(RoundFailure::ToolCallRefusal(format!(
                "{first}; corrective reissue refused by request budget"
            )));
        }
        match self.round_once(&correction, tools) {
            Err(RoundFailure::ToolCallRefusal(second)) => Err(RoundFailure::ToolCallRefusal(
                format!("{first}; one corrective reissue exhausted: {second}"),
            )),
            other => other,
        }
    }
}

#[cfg(test)]
mod tests;
