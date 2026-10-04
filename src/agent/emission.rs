use super::*;

impl CodingAgent {
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
            .result(
                turn,
                round,
                call,
                self.backend.tool_error_prefix(),
                self.output_caps.turn,
            )
            .map_err(|error| error.to_string())
    }
}
