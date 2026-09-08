//! Final-summary resolution for completed tool rounds.

use super::*;

impl CodingAgent {
    pub(super) fn resolve_summary(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        tools: &[crate::tools::ToolSpec],
        attempt: &mut AttemptState,
    ) -> Result<String, AgentError> {
        if !attempt.current.text.trim().is_empty() {
            self.check_round_limit(store, reserved, &attempt.rounds)?;
            if let Some((_, message)) = malformed_tool_call::classify(
                &attempt.current.text,
                attempt.current.calls.len(),
                self.allow_shell,
            ) {
                // A reply that looks like a tool call but parses to none is a collapsed
                // turn, not a finished one: persist it as a terminal failure so the
                // session keeps the rounds and the CLI keeps the nonzero exit.
                // The collapsed turn keeps its typed failure; the verdict rides the error.
                let mut error = self.dead(
                    store,
                    reserved,
                    MALFORMED_TOOL_CALL_KEY,
                    &message,
                    &attempt.rounds,
                );
                if error.key == MALFORMED_TOOL_CALL_KEY {
                    error.terminal_outcome = Some(MALFORMED_TOOL_CALL_KEY);
                }
                return Err(error);
            }
            return Ok(std::mem::take(&mut attempt.current.text));
        }
        self.forced_summary(store, reserved, tools, attempt)
    }

    fn forced_summary(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        tools: &[crate::tools::ToolSpec],
        attempt: &mut AttemptState,
    ) -> Result<String, AgentError> {
        self.enforce_usage(store, reserved, &attempt.rounds, &attempt.usage)?;
        attempt.requests.push(assistant_request(&attempt.current));
        attempt.requests.push(final_summary_request());
        self.check_request_budget(store, reserved, &attempt.requests, tools, &attempt.rounds)?;
        let forced =
            self.run_final_round(store, reserved, &attempt.requests, tools, &attempt.rounds)?;
        self.validate_forced_summary(store, reserved, &mut attempt.ids, &attempt.rounds, &forced)?;
        attempt.usage.assistant_bytes = attempt
            .usage
            .assistant_bytes
            .saturating_add(forced.text.len());
        self.enforce_usage(store, reserved, &attempt.rounds, &attempt.usage)?;
        self.check_round_limit(store, reserved, &attempt.rounds)?;
        Ok(forced.text)
    }

    fn validate_forced_summary(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        ids: &mut std::collections::HashSet<String>,
        rounds: &[RoundRecord],
        forced: &LlmResult,
    ) -> Result<(), AgentError> {
        let (calls, refused) = validate_calls(ids, forced, self.allow_shell)
            .map_err(|error| self.dead(store, reserved, "invalid-tool-call", &error, rounds))?;
        if !calls.is_empty() || !refused.is_empty() {
            return Err(self.dead(
                store,
                reserved,
                "invalid-tool-call",
                "final summary round asked for tools again; giving up",
                rounds,
            ));
        }
        finish_check(forced)
            .map_err(|error| self.dead(store, reserved, "finish-reason", &error, rounds))?;
        if forced.text.trim().is_empty() {
            return Err(self.dead(
                store,
                reserved,
                "empty-final-output",
                "no summary text from the model",
                rounds,
            ));
        }
        // The forced summary is the reply of record for a collapsed turn, so the same
        // malformed-tool-call detector as the normal round applies: a summary that
        // answers with invoke markup is a failed turn, not a finished one.
        if let Some((_, message)) =
            malformed_tool_call::classify(&forced.text, forced.calls.len(), self.allow_shell)
        {
            return Err(self.dead(store, reserved, MALFORMED_TOOL_CALL_KEY, &message, rounds));
        }
        Ok(())
    }
}
