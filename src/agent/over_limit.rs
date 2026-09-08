//! One-shot preservation-oriented history compaction for context recovery.

use crate::session::HistoryTurn;

/// Drops detailed prior-round material after retaining a short assistant digest.
///
/// Prompts are deliberately never changed: the current prompt is separate from history
/// and prior prompts remain useful routing context.
pub(crate) fn compact_history(history: &mut Vec<HistoryTurn>) -> bool {
    let mut dropped = false;
    for turn in history {
        if turn.rounds.is_empty() {
            continue;
        }
        if turn.summary.is_empty() {
            let mut summary = String::new();
            for round in &turn.rounds {
                if !summary.is_empty() {
                    summary.push('\n');
                }
                summary.extend(round.assistant.chars().take(200));
            }
            turn.summary = summary;
        }
        turn.rounds.clear();
        dropped = true;
    }
    dropped
}

/// Total estimated bytes including serialized tool schemas.
pub(crate) fn request_total_bytes(
    requests: &[serdes_ai::core::ModelRequest],
    tools: &[crate::tools::ToolSpec],
) -> usize {
    crate::agent::estimate_request_bytes(requests)
        .saturating_add(crate::adapter::estimate_tool_schema_bytes(tools))
}

/// Stable one-shot recovery diagnostic naming both measured attempts.
pub(crate) fn over_limit_message(
    original: usize,
    after: usize,
    context_limit: Option<u64>,
) -> String {
    let budget = crate::agent::materialization_budget(context_limit);
    format!("estimated request would be {original} bytes over the {budget}-byte heuristic guard; after one compaction attempt, still {after} bytes")
}

use super::{AttemptState, RoundFailure};
use crate::agent::{AgentError, LlmResult};
use crate::session::{ReservedRequest, RoundRecord, SessionStore};

impl super::Turn<'_> {
    /// Materialize the first request after one preservation-oriented recovery attempt.
    pub(super) fn preflight_recovery(
        &self,
        store: &SessionStore,
        reserved: &mut ReservedRequest,
    ) -> Result<Vec<serdes_ai::core::ModelRequest>, AgentError> {
        self.renew(store, reserved)?;
        if crate::agent::history_needs_check(&reserved.history)
            && !crate::agent::history_within(
                &reserved.history,
                crate::agent::materialization_budget(self.context_limit),
            )
        {
            compact_history(&mut reserved.history);
        }
        let mut requests = self.materialize_requests(reserved);
        let tools = crate::tools::tool_specs(self.allow_shell);
        if !crate::agent::round_budget_exceeded(&requests, &tools, self.context_limit) {
            return Ok(requests);
        }
        let original = request_total_bytes(&requests, &tools);
        compact_history(&mut reserved.history);
        requests = self.materialize_requests(reserved);
        if crate::agent::round_budget_exceeded(&requests, &tools, self.context_limit) {
            return Err(self.dead(
                store,
                reserved,
                "context-limit",
                &over_limit_message(
                    original,
                    request_total_bytes(&requests, &tools),
                    self.context_limit,
                ),
                &[],
            ));
        }
        Ok(requests)
    }

    pub(super) fn provider_round_with_recovery(
        &self,
        store: &SessionStore,
        reserved: &mut ReservedRequest,
        tools: &[crate::tools::ToolSpec],
        attempt: &mut AttemptState,
    ) -> Result<LlmResult, AgentError> {
        match self.profiled_round(
            &attempt.requests,
            tools,
            "model_call_before",
            "model_call_after",
            attempt.rounds.len() + 1,
            &attempt.usage,
        ) {
            Ok(reply) => Ok(reply),
            Err(RoundFailure::ContextLimit(first)) => {
                self.compact_provider_context(reserved, &mut attempt.requests);
                match self.profiled_round(
                    &attempt.requests,
                    tools,
                    "model_call_before",
                    "model_call_after",
                    attempt.rounds.len() + 1,
                    &attempt.usage,
                ) {
                    Ok(reply) => Ok(reply),
                    Err(RoundFailure::ContextLimit(second)) => Err(self
                        .provider_context_limit_dead(
                            store,
                            reserved,
                            &first,
                            &second,
                            &attempt.rounds,
                        )),
                    Err(failure) => {
                        Err(self.round_failure(store, reserved, failure, &attempt.rounds))
                    }
                }
            }
            Err(failure) => Err(self.round_failure(store, reserved, failure, &attempt.rounds)),
        }
    }

    /// Compact persisted history after a provider explicitly rejects its context length.
    /// The live suffix (tool returns already produced this turn) remains intact.
    pub(super) fn compact_provider_context(
        &self,
        reserved: &mut ReservedRequest,
        requests: &mut Vec<serdes_ai::core::ModelRequest>,
    ) {
        let base_len = self.materialize_requests(reserved).len();
        let suffix = requests[base_len..].to_vec();
        compact_history(&mut reserved.history);
        *requests = self.materialize_requests(reserved);
        requests.extend(suffix);
    }

    pub(super) fn provider_context_limit_dead(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        first: &str,
        second: &str,
        rounds: &[RoundRecord],
    ) -> AgentError {
        self.dead(
            store,
            reserved,
            "context-limit",
            &format!("provider context-limit attempt 1: {first}; retry after compaction: {second}"),
            rounds,
        )
    }

    pub(super) fn recover_request_budget(
        &self,
        store: &SessionStore,
        reserved: &mut ReservedRequest,
        requests: &mut Vec<serdes_ai::core::ModelRequest>,
        tools: &[crate::tools::ToolSpec],
        rounds: &[RoundRecord],
    ) -> Result<(), AgentError> {
        if !crate::agent::round_budget_exceeded(requests, tools, self.context_limit) {
            return Ok(());
        }
        let original = request_total_bytes(requests, tools);
        self.compact_provider_context(reserved, requests);
        if crate::agent::round_budget_exceeded(requests, tools, self.context_limit) {
            return Err(self.dead(
                store,
                reserved,
                "context-limit",
                &over_limit_message(
                    original,
                    request_total_bytes(requests, tools),
                    self.context_limit,
                ),
                rounds,
            ));
        }
        Ok(())
    }
}
