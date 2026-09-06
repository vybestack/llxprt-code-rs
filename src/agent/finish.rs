use crate::adapter::LlmResult;
use serdes_ai::core::FinishReason;

/// The remediation text for a completion cut off by the output-token cap. Shared by the
/// fatal path and the exhausted single re-issue (issue 153), so the hint a reader of one
/// failure sees never drifts from the other.
pub(super) const LENGTH_TRUNCATION_MESSAGE: &str = "completion truncated by max output tokens (finish_reason length); the model hit maxOutputTokens and did not finish; raise maxOutputTokens in the profile or split the work";

/// Check one round's completion reason before any tool executes.
pub fn finish_check(result: &LlmResult) -> Result<(), String> {
    match result.finish_reason.as_ref() {
        Some(FinishReason::Stop | FinishReason::EndTurn | FinishReason::StopSequence)
            if result.calls.is_empty() =>
        {
            Ok(())
        }
        Some(FinishReason::Stop | FinishReason::EndTurn | FinishReason::StopSequence) => {
            Err("completion says stop but includes tool calls".into())
        }
        Some(FinishReason::ToolCall) if !result.calls.is_empty() => Ok(()),
        Some(FinishReason::ToolCall) => {
            Err("completion says tool_call but has no tool calls".into())
        }
        Some(FinishReason::Length) => Err(LENGTH_TRUNCATION_MESSAGE.into()),
        Some(FinishReason::ContentFilter) => {
            Err("completion blocked (finish_reason content_filter)".into())
        }
        Some(FinishReason::Error) => Err("model returned an error completion".into()),
        Some(FinishReason::Other(raw)) => Err(format!(
            "unknown finish_reason {}",
            crate::redact::scrub_and_bound_diagnostic(raw)
        )),
        None => Err("missing finish_reason".into()),
    }
}
