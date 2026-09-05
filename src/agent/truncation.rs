//! One bounded re-issue of a turn's first completion when the output cap truncates it
//! (issue 153).
//!
//! A single over-long completion is a per-turn event, but it used to end the whole
//! session: the first `finish_reason: length` on a turn that had produced no tool call
//! failed the run even though the next attempt on the same prompt usually succeeds. When
//! no tool has run, nothing is lost by asking again, so the truncated completion is
//! dropped (it never reaches the request list or the transcript) and the turn is
//! re-issued once with a terse nudge. Exactly one re-issue runs, never a loop: a second
//! truncation keeps the typed finish-reason failure and its remediation text and only
//! adds the [`TRUNCATED_OUTPUT_RETRIED_KEY`] terminal outcome, so a supervisor can tell
//! an exhausted retry from an immediate failure. A truncation after tool calls keeps the
//! existing fatal path unchanged, because mid-work truncation loses tool context.

use super::*;
use serdes_ai::core::FinishReason;

/// Terse reminder appended for the single re-issue: the discarded reply was cut by the
/// output cap, so the replacement must stay short and act.
const NUDGE: &str = "Your previous reply hit the maxOutputTokens cap and was discarded. \
                     Reply again, much shorter: no preamble and no restating of the plan; \
                     call a tool, or give the final summary.";

/// Whether a completion was cut off by the output-token cap.
pub fn is_output_truncation(result: &LlmResult) -> bool {
    matches!(result.finish_reason.as_ref(), Some(FinishReason::Length))
}

/// Whether a truncation is retryable: the attempt has persisted no tool round yet, so
/// re-issuing the turn loses no tool context. `rounds` are the attempt's persisted rounds.
pub fn retryable(rounds: &[RoundRecord], result: &LlmResult) -> bool {
    rounds.is_empty() && is_output_truncation(result)
}

/// The user-side nudge request for the single re-issue. The re-issue reuses the turn's own
/// request list, which already carries injected instructions (the forced-summary request,
/// the budget notices), so the nudge is one more request part rather than a history
/// mutation: the truncated completion itself is never replayed as an assistant part.
fn nudge_request() -> serdes_ai::core::ModelRequest {
    user_request(NUDGE)
}

impl CodingAgent {
    /// Issue the turn's first completion, re-issuing it exactly once when the output cap
    /// truncates it before any tool call. `requests` is the turn's live request list: the
    /// nudge is appended to it and stays for the rest of the turn, while the truncated
    /// completion is dropped, so its bytes never count against the turn's assistant cap.
    pub(super) fn first_completion(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        requests: &mut Vec<serdes_ai::core::ModelRequest>,
        tools: &[crate::tools::ToolSpec],
    ) -> Result<LlmResult, AgentError> {
        let usage = TurnUsage {
            assistant_bytes: 0,
            args_bytes: 0,
            output_bytes: 0,
            total_calls: 0,
        };
        let first = self.opening_round(store, reserved, requests, tools, &usage)?;
        if !retryable(&[], &first) {
            return Ok(first);
        }
        requests.push(nudge_request());
        self.check_request_budget(store, reserved, requests, tools, &[])?;
        self.renew(store, reserved)?;
        let second = self.opening_round(store, reserved, requests, tools, &usage)?;
        if is_output_truncation(&second) {
            return Err(self.exhausted_retry(store, reserved));
        }
        Ok(second)
    }

    /// One model call of the turn's opening, with the shared profiling and failure mapping.
    fn opening_round(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        requests: &[serdes_ai::core::ModelRequest],
        tools: &[crate::tools::ToolSpec],
        usage: &TurnUsage,
    ) -> Result<LlmResult, AgentError> {
        self.profiled_round(
            requests,
            tools,
            "model_call_before",
            "model_call_after",
            1,
            usage,
        )
        .map_err(|failure| self.round_failure(store, reserved, failure, &[]))
    }

    /// The single re-issue also truncated: keep the typed finish-reason failure with its
    /// remediation text and declare the distinct terminal outcome. A persistence
    /// escalation inside `dead` keeps its own error key, so the outcome is attached only
    /// when the finish-reason failure itself is the one surfaced.
    fn exhausted_retry(&self, store: &SessionStore, reserved: &ReservedRequest) -> AgentError {
        let mut error = self.dead(
            store,
            reserved,
            "finish-reason",
            finish::LENGTH_TRUNCATION_MESSAGE,
            &[],
        );
        if error.key == "finish-reason" {
            error.terminal_outcome = Some(TRUNCATED_OUTPUT_RETRIED_KEY);
        }
        error
    }
}

#[cfg(test)]
mod tests;
