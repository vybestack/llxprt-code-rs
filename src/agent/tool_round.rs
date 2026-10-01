use super::*;

impl super::Turn<'_> {
    pub(super) fn execute_tool_round(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        config: &crate::tools::ToolConfig,
        attempt: &mut AttemptState,
    ) -> Result<bool, AgentError> {
        self.check_round_limit(store, reserved, &attempt.rounds)?;
        self.check_time_limit(store, reserved, &attempt.rounds)?;
        let (mut calls, refused) =
            validate_calls(&mut attempt.ids, &attempt.current, self.allow_shell).map_err(
                |error| {
                    self.dead(
                        store,
                        reserved,
                        "invalid-tool-call",
                        &error,
                        &attempt.rounds,
                    )
                },
            )?;
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
        refuse_unknown_tools(self.allow_shell, attempt, &mut round, &refused);
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
            in_flight::mark(store, &call.name);
            let outcome =
                self.execute_one_call(config, store, attempt, round, call, (index, calls.len()));
            in_flight::clear(store);
            if let Err(failure) = outcome {
                // Calls enter the round only after execution and result admission.
                // Publish that prefix on failure too, never the unexecuted suffix.
                // SessionStore::fail strips live projections from its durable copy.
                if !round.calls.is_empty() {
                    attempt.rounds.push(RoundRecord {
                        assistant: std::mem::take(&mut round.assistant),
                        calls: std::mem::take(&mut round.calls),
                    });
                }
                return Err(self.tool_failure(store, reserved, failure, &attempt.rounds));
            }
            self.update_profile_usage(&attempt.usage);
            // `output_bytes` is charged in LIVE bytes (#66), so the persisted
            // telemetry reads the size of the record the round actually kept.
            let persisted_bytes = round
                .calls
                .last()
                .map(|call| call.result.len())
                .unwrap_or_default();
            self.profile(
                "tool_exec_after",
                crate::memory_profile::EventData {
                    round_index: Some(round_index as u64),
                    tool_result_persisted_bytes: Some(persisted_bytes as u64),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn later_invalid_call_preserves_executed_prefix() {
        let _home = crate::agent::tests::shared_config_home();
        let cwd = tempfile::tempdir().unwrap();
        let store =
            SessionStore::load(&crate::session::SessionId::parse("later-invalid-prefix").unwrap())
                .unwrap();
        let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
        let agent = CodingAgent::with_backend(
            Box::new(crate::agent::tests::MockBackend::new(vec![])),
            cwd.path().to_path_buf(),
            false,
        );
        let turn = Turn::new(&agent).unwrap();
        let config = turn.tools_config(false).unwrap();
        let write = ToolCall {
            id: "write-prefix".into(),
            name: "write_file".into(),
            args_json: r#"{"path":"written.txt","content":"evidence"}"#.into(),
        };
        // Exercise the executor's Invalid failure arm directly: normal provider
        // validation rejects malformed batches before any tool is executed.
        let invalid = ToolCall {
            id: "invalid".into(),
            name: "write_file".into(),
            args_json: "[".into(),
        };
        let mut attempt = AttemptState {
            requests: Vec::new(),
            rounds: Vec::new(),
            current: LlmResult {
                text: "working".into(),
                calls: vec![],
                finish_reason: None,
                usage: Default::default(),
            },
            ids: Default::default(),
            usage: TurnUsage {
                assistant_bytes: 0,
                args_bytes: 0,
                output_bytes: 0,
                total_calls: 0,
            },
            budget_exhausted: false,
        };
        let mut round = RoundRecord {
            assistant: "working".into(),
            calls: Vec::new(),
        };
        let error = turn
            .execute_calls(
                &config,
                &mut attempt,
                &mut round,
                &[write.clone(), invalid, write.clone()],
                &store,
                &reserved,
            )
            .unwrap_err();
        assert_eq!(error.key, "invalid-tool-call");
        assert_eq!(
            std::fs::read(cwd.path().join("written.txt")).unwrap(),
            b"evidence"
        );
        assert_eq!(attempt.usage.total_calls, 1);
        let snapshot = store.snapshot().unwrap();
        let branch = snapshot
            .branches
            .iter()
            .find(|b| b.branch_id == reserved.branch_id)
            .unwrap();
        assert_written_prefix(branch, &write);
        let reopened =
            SessionStore::load(&crate::session::SessionId::parse("later-invalid-prefix").unwrap())
                .unwrap();
        assert_eq!(
            serde_json::to_value(reopened.snapshot().unwrap().branches).unwrap(),
            serde_json::to_value(snapshot.branches).unwrap()
        );
    }

    fn assert_written_prefix(branch: &crate::session::BranchRecord, write: &ToolCall) {
        assert_eq!(branch.lifecycle, crate::session::Lifecycle::Failed);
        assert_eq!(branch.rounds.len(), 1);
        assert_eq!(branch.rounds[0].calls.len(), 1);
        let record = &branch.rounds[0].calls[0];
        assert_eq!(record.id, write.id);
        assert_eq!(record.args, write.args_json);
        assert_eq!(record.name, write.name);
        assert!(record.ok);
        assert!(!record.refused);
        assert_eq!(record.result, "wrote 8 bytes to written.txt");
        assert!(record.result_live.is_empty());
    }
}
