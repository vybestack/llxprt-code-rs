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
        let mut calls = validate_calls(&mut attempt.ids, &attempt.current).map_err(|error| {
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
        self.profile_store(store, "session_written", attempt.rounds.len(), None)?;
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
            if known_tool(&call.name, self.allow_shell) {
                in_flight::mark(store, &call.name);
            }
            let outcome =
                self.execute_one_call(config, store, attempt, round, call, (index, calls.len()));
            in_flight::clear(store);
            outcome
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

    /// Execute one tool call of a round and record it. `index`/`total` decide
    /// whether the budget notice rides on this result (last call of the round).
    /// Failures carry their kind so the caller keeps the right error code.
    fn execute_one_call(
        &self,
        config: &crate::tools::ToolConfig,
        store: &SessionStore,
        attempt: &mut AttemptState,
        round: &mut RoundRecord,
        call: &ToolCall,
        position: (usize, usize),
    ) -> Result<(), ToolCallFailure> {
        // `position` is `(index, total)` packed into one argument so the call site and
        // the signature stay inside clippy's argument budget now that the session store
        // handle is threaded in for pre-entry compaction.
        let (index, total) = position;
        let remaining_output = self
            .output_caps
            .turn
            .saturating_sub(attempt.usage.output_bytes);
        if remaining_output == 0 {
            return Err(ToolCallFailure::OutputCap);
        }
        let parsed = parse_object_args(call).map_err(ToolCallFailure::Invalid)?;
        let (ok, raw_text) = if known_tool(&call.name, self.allow_shell) {
            crate::tools::execute_tool_with_limit(
                &self.cwd,
                &call.name,
                parsed,
                config,
                remaining_output,
            )
        } else {
            (false, helpers::naming_failure(self.allow_shell, &call.name))
        };
        let scrubbed = crate::redact::truncate_utf8(
            crate::redact::scrub_secrets(&raw_text, &self.secrets),
            config.max_output_bytes.min(remaining_output),
        );
        attempt.usage.total_calls += 1;
        let notice = if index + 1 == total {
            budget_notice(self.max_tool_calls, attempt.usage.total_calls)
        } else {
            String::new()
        };
        // The notice must survive truncation, so reserve its bytes (plus the blank
        // line that carries it) first; with no notice there is nothing to reserve.
        let body_budget = if notice.is_empty() {
            remaining_output
        } else {
            remaining_output.saturating_sub(notice.len().saturating_add(2))
        };
        let text = crate::redact::truncate_utf8(scrubbed, body_budget);
        let text = if notice.is_empty() {
            text
        } else {
            format!("{text}\n\n{notice}")
        };
        // Pre-entry compaction (#39): a bulk tool result is digested before it joins the
        // request list and the round, so neither the next provider request nor the
        // checkpointed transcript ever carries raw bulk bytes.
        let text = store
            .compact_tool_result(&call.name, &text)
            .map_err(|error| ToolCallFailure::Invalid(error.to_string()))?;
        attempt.usage.output_bytes = attempt.usage.output_bytes.saturating_add(text.len());
        attempt
            .requests
            .push(tool_return_request(&call.name, &call.id, ok, &text));
        round.calls.push(tool_call_record(call, ok, text));
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
                &format!(
                    "turn tool output reached the {} byte cap",
                    self.output_caps.turn
                ),
                rounds,
            ),
        }
    }
}

/// Parse a tool call's argument JSON strictly: it must be a JSON object. A malformed raw
/// argument (which the vendored transport preserves verbatim) fails here, so it can never become a
/// successful `{}` round.
pub fn parse_object_args(call: &ToolCall) -> Result<JsonValue, String> {
    match serde_json::from_str::<JsonValue>(&call.args_json) {
        Ok(v) if v.is_object() => Ok(v),
        Ok(_) => Err(format!(
            "tool call {}: arguments must be a JSON object",
            call.name
        )),
        Err(e) => Err(format!(
            "tool call {}: invalid argument JSON: {e}",
            call.name
        )),
    }
}
