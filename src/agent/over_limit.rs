//! One-shot preservation-oriented history compaction for context recovery.

pub(super) mod content;

use crate::session::HistoryTurn;

/// Reclaims redundant prior-round assistant narration using the completed summary.
/// Tool calls, admitted result digests and prompts stay addressable and paired. A
/// turn without a completed semantic summary is not eligible for this reclamation.
pub(crate) fn compact_history(history: &mut [HistoryTurn]) -> bool {
    let mut changed = false;
    for turn in history {
        if turn.summary.is_empty() {
            continue;
        }
        for round in &mut turn.rounds {
            if round.assistant != turn.summary && !round.assistant.is_empty() {
                round.assistant.clear();
                changed = true;
            }
        }
    }
    changed
}

/// Replace only previously observed results with their successfully admitted durable
/// projection. The newest tool round is mandatory live evidence; neither it nor the
/// call/return framing, instructions, correction notices or usage counters are changed.
fn compact_observed_results(
    requests: &mut [serdes_ai::core::ModelRequest],
    rounds: &[crate::session::RoundRecord],
) -> bool {
    use serdes_ai::core::ModelRequestPart;
    let mut changed = false;
    let observed = rounds.iter().take(rounds.len().saturating_sub(1));
    let calls: std::collections::HashMap<_, _> = observed
        .flat_map(|round| &round.calls)
        .filter(|call| call.result.len() < call.result_live.len())
        .map(|call| (call.id.as_str(), call))
        .collect();
    for part in requests.iter_mut().flat_map(|request| &mut request.parts) {
        let ModelRequestPart::ToolReturn(result) = part else {
            continue;
        };
        let Some(call) = result.tool_call_id.as_deref().and_then(|id| calls.get(id)) else {
            continue;
        };
        let replacement = if call.ok {
            serdes_ai::core::ToolReturnPart::success(&call.name, &call.result).content
        } else {
            serdes_ai::core::ToolReturnPart::error(&call.name, &call.result).content
        };
        if result.content != replacement {
            result.content = replacement;
            changed = true;
        }
    }
    changed
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
        let mut requests = self.materialize_requests(reserved);
        let tools = crate::tools::tool_specs(self.allow_shell);
        if !crate::agent::round_budget_exceeded(&requests, &tools, self.context_limit) {
            return Ok(requests);
        }
        let original = request_total_bytes(&requests, &tools);
        self.compact_provider_context(reserved, &mut requests, &[]);
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
        phases: (&'static str, &'static str),
    ) -> Result<LlmResult, AgentError> {
        match self.profiled_round(
            &attempt.requests,
            tools,
            phases.0,
            phases.1,
            attempt.rounds.len() + 1,
            &attempt.usage,
        ) {
            Ok(reply) => Ok(reply),
            Err(RoundFailure::Model(first)) if super::is_context_limit_error(&first) => {
                if !self.compact_provider_context(reserved, &mut attempt.requests, &attempt.rounds)
                {
                    return Err(self.provider_context_unchanged_dead(
                        store,
                        reserved,
                        &first,
                        &attempt.rounds,
                    ));
                }
                self.check_request_budget(
                    store,
                    reserved,
                    &attempt.requests,
                    tools,
                    &attempt.rounds,
                )?;
                match self.profiled_round(
                    &attempt.requests,
                    tools,
                    phases.0,
                    phases.1,
                    attempt.rounds.len() + 1,
                    &attempt.usage,
                ) {
                    Ok(reply) => Ok(reply),
                    Err(RoundFailure::Model(second)) if super::is_context_limit_error(&second) => {
                        Err(self.provider_context_limit_dead(
                            store,
                            reserved,
                            &first,
                            &second,
                            &attempt.rounds,
                        ))
                    }
                    Err(failure) => {
                        Err(self.round_failure(store, reserved, failure, &attempt.rounds))
                    }
                }
            }
            Err(failure) => Err(self.round_failure(store, reserved, failure, &attempt.rounds)),
        }
    }

    /// Commit recovery only for an eligible strict content reduction. Internal
    /// serialization timestamps still count in admission, never retry eligibility.
    pub(super) fn compact_provider_context(
        &self,
        reserved: &mut ReservedRequest,
        requests: &mut Vec<serdes_ai::core::ModelRequest>,
        rounds: &[RoundRecord],
    ) -> bool {
        let mut history = reserved.clone();
        let history_changed = compact_history(&mut history.history);
        let mut candidate = requests.clone();
        if history_changed {
            let base_len = self.materialize_requests(reserved).len();
            let mut prefix = self.materialize_requests(&history);
            content::preserve_unchanged_prefix(&requests[..base_len], &mut prefix);
            candidate.splice(..base_len, prefix);
        }
        let base_len = self.materialize_requests(&history).len();
        let results_changed = compact_observed_results(&mut candidate[base_len..], rounds);
        if !(history_changed || results_changed) || !content::content_shrank(requests, &candidate) {
            return false;
        }
        reserved.history = history.history;
        *requests = candidate;
        true
    }

    pub(super) fn provider_context_unchanged_dead(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        first: &str,
        rounds: &[RoundRecord],
    ) -> AgentError {
        self.dead(
            store, reserved, "context-limit",
            &format!("provider context-limit: {first}; compaction did not shrink the request; no retry sent"),
            rounds,
        )
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
        self.compact_provider_context(reserved, requests, rounds);
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
