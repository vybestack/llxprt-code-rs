use super::*;

impl CodingAgent {
    /// Override the per-turn round cap.
    pub fn with_max_rounds(mut self, max_rounds: usize) -> CodingAgent {
        self.max_rounds = max_rounds;
        self
    }

    /// Override the agent's conservative per-request context budget.
    pub fn with_context_limit(mut self, context_limit: Option<u64>) -> CodingAgent {
        self.context_limit = context_limit;
        self
    }

    /// Attach the optional process-memory event sink.
    pub fn with_profiler(
        mut self,
        profiler: Option<crate::memory_profile::Profiler>,
    ) -> CodingAgent {
        self.profiler = profiler;
        self
    }

    /// Enable selected live transcript categories. Default agents emit nothing.
    pub fn with_emission(mut self, categories: Vec<crate::transcript::Emit>) -> Self {
        self.emitter = std::sync::Mutex::new(crate::transcript::Emitter::new(categories));
        self
    }

    pub(super) fn emit_response(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        rounds: &[RoundRecord],
        reply: &LlmResult,
    ) -> Result<(), AgentError> {
        self.emitter
            .lock()
            .expect("transcript mutex poisoned")
            .response(reserved.turn, rounds.len() + 1, reply, &self.secrets)
            .map_err(|error| self.dead(store, reserved, "transcript", &error.to_string(), rounds))
    }

    pub(super) fn emit_result(
        &self,
        turn: u32,
        round: usize,
        call: &crate::session::ToolCallRecord,
    ) -> Result<(), String> {
        self.emitter
            .lock()
            .expect("transcript mutex poisoned")
            .result(turn, round, call, self.backend.tool_error_prefix())
            .map_err(|error| error.to_string())
    }
}
