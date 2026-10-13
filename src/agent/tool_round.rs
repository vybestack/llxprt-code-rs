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
        let current = attempt
            .current
            .take()
            .expect("tool round owns an unassembled reply");
        let mut calls = validate_calls(&mut attempt.ids, &current).map_err(|error| {
            self.dead(
                store,
                reserved,
                "invalid-tool-call",
                &error,
                &attempt.rounds,
            )
        })?;
        attempt.requests.push(assistant_request(&current));
        // Enforce the tool-call budget by executing only what fits: the model
        // gets explicit refusals for the rest, and the turn resolves through a
        // forced summary instead of dying mid-work.
        let skipped = split_over_budget(self.max_tool_calls, &attempt.usage, &mut calls);
        let truncated = !skipped.is_empty();
        if truncated {
            attempt.budget_exhausted = true;
        }
        let mut round = RoundRecord {
            thinking: current.thinking,
            continuation: current.continuation,
            assistant: current.text,
            calls: Vec::new(),
        };
        self.execute_calls(config, attempt, &mut round, &calls, store, reserved)?;
        for call in &skipped {
            self.observations
                .borrow_mut()
                .push_tool(crate::telemetry::ToolExecution {
                    round: attempt.rounds.len() as u64 + 1,
                    name: if known_tool(&call.name, self.allow_shell) {
                        call.name.clone()
                    } else {
                        "unregistered".into()
                    },
                    exit_class: "budget-refused".into(),
                    wall_ms: 0,
                    output_bytes: 0,
                    truncated: false,
                });
        }
        refuse_over_budget(self.max_tool_calls, attempt, &mut round, &skipped);
        let emitted_prefix = calls.len();
        attempt.rounds.push(round);
        for call in &attempt.rounds.last().expect("recorded round").calls[emitted_prefix..] {
            store
                .complete_provider_tool(reserved, call)
                .map_err(AgentError::from_store)?;
            self.emit_result(reserved.turn, attempt.rounds.len(), call)
                .map_err(|error| {
                    self.dead(store, reserved, "transcript", &error, &attempt.rounds)
                })?;
        }
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
            if known_tool(&call.name, self.allow_shell) {
                in_flight::mark(store, &call.name);
            }
            let outcome = store
                .with_tool_lease(reserved, || {
                    self.execute_one_call(config, store, attempt, round, call, (index, calls.len()))
                })
                .unwrap_or_else(|error| Err(ToolCallFailure::Session(error)));
            in_flight::clear(store);
            if let Err(failure) = outcome {
                // Calls enter the round only after execution and result admission.
                // Publish that prefix on failure too, never the unexecuted suffix.
                // SessionStore::fail strips live projections from its durable copy.
                if !round.calls.is_empty()
                    || round.continuation.is_some()
                    || !round.thinking.is_empty()
                {
                    attempt.rounds.push(RoundRecord {
                        thinking: std::mem::take(&mut round.thinking),
                        continuation: round.continuation.take(),
                        assistant: std::mem::take(&mut round.assistant),
                        calls: std::mem::take(&mut round.calls),
                    });
                }
                return Err(self.tool_failure(store, reserved, failure, &attempt.rounds));
            }
            store
                .complete_provider_tool(reserved, round.calls.last().expect("admitted call record"))
                .map_err(AgentError::from_store)?;
            if let Err(error) = self.emit_result(
                reserved.turn,
                round_index,
                round.calls.last().expect("admitted call record"),
            ) {
                attempt.rounds.push(RoundRecord {
                    thinking: std::mem::take(&mut round.thinking),
                    continuation: round.continuation.take(),
                    assistant: std::mem::take(&mut round.assistant),
                    calls: std::mem::take(&mut round.calls),
                });
                return Err(self.dead(store, reserved, "transcript", &error, &attempt.rounds));
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
            ToolCallFailure::Session(error) => self.dead_coded(
                store,
                reserved,
                crate::envelope::Code::Session,
                "session",
                &error.to_string(),
                rounds,
            ),
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
        admit_fixture(&store, &reserved, &write);
        let mut attempt = AttemptState {
            requests: Vec::new(),
            rounds: Vec::new(),
            current: Some(fixture_reply()),
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
            thinking: String::new(),
            continuation: None,
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

    fn admit_fixture(store: &SessionStore, reserved: &ReservedRequest, write: &ToolCall) {
        let permit = store
            .provider_intent(
                reserved,
                crate::context_gateway::SendIntent {
                    identity: String::new(),
                    branch: reserved.branch_id.clone(),
                    owner: reserved.owner.clone(),
                    epoch: 1,
                    context_version: 1,
                    request_hash: crate::context_gateway::digest(b"fixture"),
                    contract_hash: crate::context_gateway::digest(b"contract"),
                    class: crate::context_gateway::ProviderClass::AtMostOneAttempt,
                    kind: crate::context_gateway::TurnKind::Conversation,
                    allowance: 4096,
                },
            )
            .unwrap();
        let identity = store.admit_send(reserved, permit).unwrap().identity;
        let mut reply = fixture_reply();
        reply.calls = vec![write.clone()];
        store
            .admit_provider_response(
                reserved,
                &identity,
                serde_json::to_vec(&reply).unwrap(),
                vec![crate::context_gateway::ExecutionIntent {
                    id: write.id.clone(),
                    tool: write.name.clone(),
                    args: write.args_json.clone(),
                    read_only: false,
                    completion: None,
                }],
            )
            .unwrap();
    }

    fn fixture_reply() -> LlmResult {
        LlmResult {
            continuation: None,
            thinking: String::new(),
            text: "working".into(),
            calls: vec![],
            finish_reason: None,
            usage: Default::default(),
        }
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
