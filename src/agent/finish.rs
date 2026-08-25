use crate::adapter::LlmResult;
use serdes_ai::core::FinishReason;

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
        Some(FinishReason::Length) => Err(
            "completion truncated by max output tokens (finish_reason length); the model hit maxOutputTokens and did not finish; raise maxOutputTokens in the profile or split the work".into(),
        ),
        Some(FinishReason::ContentFilter) => {
            Err("completion blocked (finish_reason content_filter)".into())
        }
        Some(FinishReason::Error) => Err("model returned an error completion".into()),
        Some(FinishReason::Other(raw)) => Err(format!(
            "unknown finish_reason {:?}",
            crate::redact::scrub_and_bound_diagnostic(raw)
        )),
        None => Err("missing finish_reason".into()),
    }
}
