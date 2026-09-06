use crate::adapter::LlmResult;
use crate::agent::CompletedRun;
use crate::limits::prompt_digest;
use crate::session::ReservedRequest;
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

/// Issue #144 guard: a zero-call completion is only a channel failure when the
/// model emitted tool-call markup as plain text (the vLLM chat template that
/// swallowed the tools array did exactly that), so the guard keys on the
/// `<tool_call` fingerprint in the summary instead of the bare count;
/// CLI-forced summaries produced by the oversized-assistant split carry no
/// markup and stay declared ok outcomes. The guard fires before
/// complete_attempt because a persisted Completed branch can no longer be
/// failed through dead()/store.fail.
pub(crate) fn zero_call_channel_failure(completed_tool_count: usize, summary: &str) -> bool {
    completed_tool_count == 0 && summary.contains("<tool_call")
}

/// Rebuild a completed run from a reserved replay request.
pub(crate) fn replayed_run(
    max_tool_calls: Option<usize>,
    reserved: &ReservedRequest,
) -> CompletedRun {
    CompletedRun {
        turn: reserved.turn,
        attempt: reserved.attempt,
        branch_id: reserved.branch_id.clone(),
        summary: reserved.summary.clone(),
        tool_count: reserved
            .rounds
            .iter()
            .flat_map(|round| round.calls.iter())
            .filter(|call| !call.refused)
            .count(),
        prompt_digest: prompt_digest(&reserved.prompt),
        status: "ok".into(),
        branch: reserved.attempt > 1,
        declared_tool_calls: max_tool_calls,
        budget_exhausted: false,
        replayed: true,
    }
}
