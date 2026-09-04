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

    pub(super) fn profiled_round(
        &self,
        requests: &[serdes_ai::core::ModelRequest],
        tools: &[crate::tools::ToolSpec],
        before: &'static str,
        after: &'static str,
        round_index: usize,
        usage: &TurnUsage,
    ) -> Result<LlmResult, RoundFailure> {
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
        let result = self.round(requests, tools);
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
        result.map_err(RoundFailure::Model)
    }

    pub(super) fn round_failure(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        failure: RoundFailure,
        rounds: &[RoundRecord],
    ) -> AgentError {
        match failure {
            RoundFailure::Model(message) => self.dead(store, reserved, "model", &message, rounds),
            RoundFailure::Profiling(error) => error,
        }
    }

    pub(super) fn profile_store(
        &self,
        store: &SessionStore,
        phase: &'static str,
        round_count: usize,
    ) -> Result<(), AgentError> {
        let metrics = store.take_profile_metrics();
        self.profile(
            phase,
            crate::memory_profile::EventData {
                session_slot_input_bytes: Some(metrics.input_bytes),
                session_slot_output_bytes: Some(metrics.output_bytes),
                round_count: Some(round_count as u64),
                ..Default::default()
            },
        )
    }
}
