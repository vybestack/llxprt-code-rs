use super::*;

impl CodingAgent {
    pub(super) fn profile(
        &self,
        phase: &'static str,
        data: crate::memory_profile::EventData,
    ) -> Result<(), AgentError> {
        match &self.profiler {
            Some(profiler) => profiler.event(phase, data).map_err(|error| {
                AgentError::new(crate::envelope::Code::Profiling, error.stage, error.message)
            }),
            None => Ok(()),
        }
    }

    pub(super) fn update_profile_usage(&self, usage: &TurnUsage) {
        if let Some(profiler) = &self.profiler {
            profiler.usage(
                usage.total_calls,
                usage.assistant_bytes,
                usage.args_bytes,
                usage.output_bytes,
            );
        }
    }

    pub(super) fn profile_store(
        &self,
        store: &SessionStore,
        phase: &'static str,
        round_count: usize,
        turn_elapsed: Option<std::time::Duration>,
    ) -> Result<(), AgentError> {
        let metrics = store.take_profile_metrics();
        self.profile(
            phase,
            crate::memory_profile::EventData {
                session_slot_input_bytes: Some(metrics.input_bytes),
                session_slot_output_bytes: Some(metrics.output_bytes),
                round_count: Some(round_count as u64),
                request_timeout_ms: self
                    .request_timeout
                    .map(|timeout| timeout.as_millis() as u64),
                turn_elapsed_ms: turn_elapsed.map(|elapsed| elapsed.as_millis() as u64),
                ..Default::default()
            },
        )
    }
}

impl super::Turn<'_> {
    pub(super) fn profiled_round(
        &self,
        requests: &[serdes_ai::core::ModelRequest],
        tools: &[crate::tools::ToolSpec],
        before: &'static str,
        after: &'static str,
        context: (&SessionStore, &ReservedRequest, usize),
        usage: &TurnUsage,
    ) -> Result<LlmResult, RoundFailure> {
        let round_index = context.2;
        self.update_profile_usage(usage);
        let call_index = self.backend.request_calls();
        let data = crate::memory_profile::EventData {
            round_index: Some(round_index as u64),
            call_index: Some(call_index as u64),
            request_estimate_bytes: Some(estimate_request_bytes(requests) as u64),
            tool_schema_estimate_bytes: Some(
                crate::adapter::estimate_tool_schema_bytes(tools) as u64
            ),
            ..Default::default()
        };
        self.profile(before, data)
            .map_err(RoundFailure::Profiling)?;
        let result = self.round(requests, tools, context);
        let mapped = result
            .as_ref()
            .ok()
            .map(|reply| crate::memory_profile::mapped_reply_bytes(reply) as u64);
        self.profile(
            after,
            crate::memory_profile::EventData {
                round_index: Some(round_index as u64),
                call_index: Some(call_index as u64),
                model_reply_mapped_bytes: mapped,
                ..Default::default()
            },
        )
        .map_err(RoundFailure::Profiling)?;
        result
    }

    pub(super) fn round_failure(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        failure: RoundFailure,
        rounds: &[RoundRecord],
    ) -> AgentError {
        match failure {
            RoundFailure::Calibration(error) => self.dead_coded(
                store, reserved, crate::envelope::Code::Turn, "request-calibration",
                &error.to_string(), rounds),
            RoundFailure::RequestBound { limit } => self.dead_coded(
                store, reserved, crate::envelope::Code::Turn, "request-bound",
                &format!("calibrated request exceeds context limit {limit}; no request sent"), rounds),
            RoundFailure::StreamInterrupted(events) => self.dead(
                store, reserved, "model",
                &format!("responses SSE transport interrupted before terminal event; accumulated {events} stream events (partial response is not completion); 3 retries exhausted"),
                rounds,
            ),
            RoundFailure::RetryStore(error) => self.dead_coded(
                store, reserved, crate::envelope::Code::Session,
                "session", &error.to_string(), rounds,
            ),
            RoundFailure::RetryTranscript(error) => self.dead(
                store, reserved, "transcript", &error.to_string(), rounds,
            ),
            RoundFailure::ToolCallRefusal(message) => {
                let mut error =
                    self.dead(store, reserved, MALFORMED_TOOL_CALL_KEY, &message, rounds);
                if error.key == MALFORMED_TOOL_CALL_KEY {
                    error.terminal_outcome = Some(MALFORMED_TOOL_CALL_KEY);
                }
                error
            }
            RoundFailure::InvalidToolCall(message) => {
                self.dead(store, reserved, "invalid-tool-call", &message, rounds)
            }
            RoundFailure::FinishReason(message) => {
                self.dead(store, reserved, "finish-reason", &message, rounds)
            }
            RoundFailure::Model(message) => self.dead(store, reserved, "model", &message, rounds),
            RoundFailure::ModelTransport(failure) => {
                let key = failure.transport_key();
                self.dead(store, reserved, "model", &failure.diagnostic(), rounds)
                    .with_envelope_code(key)
            }
            RoundFailure::TransportRetryStopped { failure, retries, reason } => {
                let message = format!("{}; {reason}; retry budget: {retries}/3 used, {} remaining",
                    failure.diagnostic(), 3 - retries);
                self.dead(store, reserved, "model", &message, rounds)
                    .with_envelope_code(failure.transport_key())
            }
            RoundFailure::TurnTime => self.time_exhausted(store, reserved, rounds),
            RoundFailure::Profiling(error) => error,
            RoundFailure::CacheOutput(error) => self.dead_coded(
                store,
                reserved,
                crate::envelope::Code::Turn,
                "cache-output-write",
                &error.to_string(),
                rounds,
            ),
        }
    }
}
