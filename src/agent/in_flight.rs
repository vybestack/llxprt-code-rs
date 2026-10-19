//! Persist validated call evidence before dispatch; normal completion clears it.
use super::*;
use crate::session::interruption::{self, InFlight, MAX_ARGS_BYTES};

pub(crate) fn mark(
    store: &SessionStore,
    reserved: &ReservedRequest,
    call: &ToolCall,
    round: usize,
    secrets: &[String],
) -> Result<(), crate::session::StoreError> {
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    interruption::mark(
        store,
        reserved,
        &InFlight {
            tool: call.name.clone(),
            id: call.id.clone(),
            turn: reserved.turn,
            round,
            started,
            args: crate::redact::truncate_utf8(
                crate::redact::scrub_secrets(&call.args_json, secrets),
                MAX_ARGS_BYTES,
            ),
        },
    )
}

pub(crate) fn clear(
    store: &SessionStore,
    reserved: &ReservedRequest,
) -> Result<(), crate::session::StoreError> {
    interruption::clear(store, reserved)
}

impl Turn<'_> {
    pub(super) fn dispatch_recorded(
        &self,
        config: &crate::tools::ToolConfig,
        session: (&SessionStore, &ReservedRequest, usize),
        call: &ToolCall,
        parsed: JsonValue,
        body_budget: usize,
    ) -> Result<(bool, String), ToolCallFailure> {
        let (store, reserved, round) = session;
        if known_tool(&call.name, self.allow_shell) {
            in_flight::mark(store, reserved, call, round, &self.secrets)
                .map_err(ToolCallFailure::Session)?;
        }
        Ok(if known_tool(&call.name, self.allow_shell) {
            crate::tools::execute_tool_with_limit(
                &self.cwd,
                &call.name,
                parsed,
                config,
                body_budget,
            )
        } else {
            (
                false,
                crate::redact::truncate_utf8(
                    helpers::naming_failure(self.allow_shell, &call.name),
                    config.max_output_bytes.min(body_budget),
                ),
            )
        })
    }
}

pub(super) fn finish(
    store: &SessionStore,
    reserved: &ReservedRequest,
    outcome: Result<(), ToolCallFailure>,
) -> Result<(), ToolCallFailure> {
    clear(store, reserved).map_err(ToolCallFailure::Session)?;
    outcome
}

#[cfg(test)]
mod tests;
