use super::*;

impl CodingAgent {
    pub(super) fn execute_tool_round(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        config: &crate::tools::ToolConfig,
        attempt: &mut AttemptState,
    ) -> Result<bool, AgentError> {
        self.check_round_limit(store, reserved, &attempt.rounds)?;
        self.check_time_limit(store, reserved, &attempt.rounds, attempt.started.elapsed())?;
        let mut calls = validate_calls(&mut attempt.ids, &attempt.current, self.allow_shell)
            .map_err(|error| {
                self.dead(
                    store,
                    reserved,
                    "invalid-tool-call",
                    &error,
                    &attempt.rounds,
                )
            })?;
        attempt.requests.push(assistant_request(&attempt.current));
        // Enforce the tool-call budget by executing only what fits: the model
        // gets explicit refusals for the rest, and the turn resolves through a
        // forced summary instead of dying mid-work.
        let skipped = split_over_budget(self.max_tool_calls, &attempt.usage, &mut calls);
        let truncated = !skipped.is_empty();
        if truncated {
            attempt.budget_exhausted = true;
        }
        let mut round = RoundRecord {
            assistant: attempt.current.text.clone(),
            calls: Vec::new(),
        };
        self.execute_calls(config, attempt, &mut round, &calls, store, reserved)?;
        refuse_over_budget(self.max_tool_calls, attempt, &mut round, &skipped);
        attempt.rounds.push(round);
        self.enforce_usage(store, reserved, &attempt.rounds, &attempt.usage)?;
        store
            .checkpoint(reserved, &attempt.rounds)
            .map_err(AgentError::from_store)?;
        self.update_profile_usage(&attempt.usage);
        self.profile_store(store, "session_written", attempt.rounds.len())?;
        Ok(truncated)
    }
    fn execute_calls(
        &self,
        config: &crate::tools::ToolConfig,
        attempt: &mut AttemptState,
        round: &mut RoundRecord,
        calls: &[ToolCall],
        store: &SessionStore,
        reserved: &ReservedRequest,
    ) -> Result<(), AgentError> {
        for (index, call) in calls.iter().enumerate() {
            self.update_profile_usage(&attempt.usage);
            let round_index = attempt.rounds.len() + 1;
            self.profile(
                "tool_exec_before",
                crate::memory_profile::EventData {
                    round_index: Some(round_index as u64),
                    round_count: Some(attempt.rounds.len() as u64),
                    ..Default::default()
                },
            )?;
            let output_before = attempt.usage.output_bytes;
            self.execute_one_call(config, attempt, round, call, index, calls.len())
                .map_err(|failure| self.tool_failure(store, reserved, failure, &attempt.rounds))?;
            self.update_profile_usage(&attempt.usage);
            self.profile(
                "tool_exec_after",
                crate::memory_profile::EventData {
                    round_index: Some(round_index as u64),
                    tool_result_persisted_bytes: Some(
                        attempt.usage.output_bytes.saturating_sub(output_before) as u64,
                    ),
                    round_count: Some(attempt.rounds.len() as u64),
                    ..Default::default()
                },
            )?;
        }
        Ok(())
    }

    fn tool_failure(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        failure: ToolCallFailure,
        rounds: &[RoundRecord],
    ) -> AgentError {
        match failure {
            ToolCallFailure::Invalid(error) => {
                self.dead(store, reserved, "invalid-tool-call", &error, rounds)
            }
            ToolCallFailure::OutputCap => self.dead(
                store,
                reserved,
                "limit",
                &format!("turn tool output reached the {MAX_TURN_OUTPUT_BYTES} byte cap"),
                rounds,
            ),
        }
    }
}
