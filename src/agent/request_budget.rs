//! Request-budget constants, estimator helpers, and their tests for the agent loop.
//!
//! The per-turn byte and round caps owned here are the same bounds the session
//! validator enforces on persisted transcripts; the free helpers below estimate the
//! complete outgoing request and materialized history so an over-budget turn is refused
//! before the backend call. Every accumulator here uses saturated arithmetic so an
//! oversized request or history can never wrap a smaller value.

use super::LlmResult;
/// Cap on the combined assistant text bytes materialized in one turn.
pub const MAX_TURN_ASSISTANT_BYTES: usize = 1024 * 1024;
/// Cap on the combined raw tool-call argument bytes in one turn.
pub const MAX_TURN_ARGS_BYTES: usize = 1024 * 1024;
/// Cap on the combined tool-result bytes materialized in one turn.
pub const MAX_TURN_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
/// Default cap on the number of assistant/tool rounds in one turn: none. Rounds are
/// uncapped unless the profile (`maxTurnsPerPrompt`) or an explicit value caps them;
/// byte/output caps and the declared tool-call and turn-time budgets still bound the
/// run.
pub const MAX_TURN_ROUNDS: usize = usize::MAX;
/// Default per-prompt tool-call budget: none. Tools are uncapped unless the profile
/// (`maxToolCallsPerPrompt`) or `--max-tool-calls` caps them; the round cap
/// (`maxTurnsPerPrompt`), the turn-time budget, and byte/output caps still bound the
/// run. The parity harness validates the envelope's `tool_calls` against the declared
/// budget, so an uncapped run must still account for every call it executed.
/// Hard cap on one model reply (bytes). A reply over this bound is refused by the
/// agent's model-call path as a typed `model` failure rather than ever becoming an
/// unbounded assistant round, so it can never be counted toward the aggregate turn caps or
/// touch session-persist. The error path persists the failure (owner cleared, lease
/// released) with a scrubbed bounded diagnostic.
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
/// Maximum UTF-8 byte length of one provider-supplied tool-call identifier.
pub const MAX_TOOL_CALL_ID_BYTES: usize = 256;
/// Maximum UTF-8 byte length of one provider-supplied tool name.
pub const MAX_TOOL_NAME_BYTES: usize = 64;

/// Bounded framing overhead (bytes) folded into the conservative preflight estimate for
/// the **complete** outgoing request on top of the message parts: the model identifier
/// (capped at [`crate::profile::MAX_MODEL_NAME_BYTES`]), the full
/// chat-completions route (base URL capped at
/// [`crate::redact::MAX_ENDPOINT_BYTES`] plus the route suffix), the serialized
/// provider-settings fields, and the HTTP/JSON request framing. Every value here is
/// bounded by a fixed over-count; the live values are never echoed.
pub const REQUEST_FIXED_OVERHEAD_BYTES: usize = 8192;
/// Per-request overhead: the role wrapper, preserved call/turn ids, and JSON framing.
pub const PER_REQUEST_OVERHEAD_BYTES: usize = 512;
/// Per-part overhead: the part role/id and JSON framing.
pub const PER_PART_OVERHEAD_BYTES: usize = 128;

/// The aggregate raw tool-call argument bytes in one round (the bytes that would be
/// persisted as the round's tool calls and materialized back into the next request).
pub fn turn_args_bytes(result: &LlmResult) -> usize {
    result.calls.iter().map(|c| c.args_json.len()).sum()
}

/// A conservative per-request byte budget for the *materialized* history the model
/// sees, derived from the profile's `context-limit` at 3 bytes per configured token.
/// This is a memory/request-size heuristic, not a tokenizer or a guarantee that a provider
/// will accept the request. `None` falls back to a fixed cap so
/// memory stays bounded even without a profile.
pub fn materialization_budget(context_limit: Option<u64>) -> usize {
    match context_limit {
        Some(n) if n > 0 => (n as usize).saturating_mul(3).min(512 * 1024 * 1024),
        _ => 32 * 1024 * 1024,
    }
}

/// Whether materialized history is worth checking (it only grows across turns).
pub fn history_needs_check(history: &[crate::session::HistoryTurn]) -> bool {
    !history.is_empty()
}

/// Whether the materialized history is inside the byte budget.
pub fn history_within(history: &[crate::session::HistoryTurn], budget: usize) -> bool {
    estimate_history_bytes(history) <= budget
}

/// Estimate the bytes a history turn will occupy when materialized (prompt + assistant
/// text + calls + results). Used only for the conservative context gate.
pub fn estimate_history_bytes(h: &[crate::session::HistoryTurn]) -> usize {
    let mut n = 0usize;
    for t in h {
        n = n
            .saturating_add(t.prompt.len())
            .saturating_add(t.summary.len());
        for r in &t.rounds {
            n = n.saturating_add(r.assistant.len());
            for c in &r.calls {
                n = n
                    .saturating_add(c.args.len())
                    .saturating_add(c.result.len());
            }
        }
        n = n.saturating_add(16 * 1024);
    }
    n
}

/// A conservative estimate of the *complete* outgoing request: every serialized part of
/// every `serdes_ai::core::ModelRequest` plus the model identifier, the route, the
/// provider-settings fields, and bounded framing/role/id overhead. This is a byte
/// estimate of `serde_json`-serialized parts alongside the fixed overhead constants, so
/// it is always a conservative over-count of the transport payload plus serialization
/// overhead, and the accumulator uses saturated arithmetic so an oversized request can
/// never wrap a smaller value.
pub fn estimate_request_bytes(requests: &[serdes_ai::core::ModelRequest]) -> usize {
    let mut n = REQUEST_FIXED_OVERHEAD_BYTES;
    for r in requests {
        n = n.saturating_add(PER_REQUEST_OVERHEAD_BYTES);
        for p in &r.parts {
            n = n
                .saturating_add(PER_PART_OVERHEAD_BYTES)
                .saturating_add(serde_json::to_vec(p).map(|v| v.len()).unwrap_or(0));
        }
    }
    n
}

/// Whether the complete outgoing request (parts + tool schemas) is over the conservative
/// context budget, so it is refused before the backend call rather than sent over budget.
pub fn round_budget_exceeded(
    requests: &[serdes_ai::core::ModelRequest],
    tools: &[crate::tools::ToolSpec],
    context_limit: Option<u64>,
) -> bool {
    estimate_request_bytes(requests)
        .saturating_add(crate::adapter::estimate_tool_schema_bytes(tools))
        > materialization_budget(context_limit)
}

/// The conservative refusal message for an over-budget outgoing request.
pub fn context_exceeded_message(
    requests: &[serdes_ai::core::ModelRequest],
    tools: &[crate::tools::ToolSpec],
    context_limit: Option<u64>,
) -> String {
    let budget = materialization_budget(context_limit);
    let total = estimate_request_bytes(requests)
        .saturating_add(crate::adapter::estimate_tool_schema_bytes(tools));
    match context_limit {
        Some(n) => format!(
            "the estimated complete outgoing request would be {total} bytes, over the configured context-limit of {n} tokens ({budget}-byte heuristic guard); no request is sent"
        ),
        None => format!(
            "the estimated complete outgoing request would be {total} bytes, over the {budget} byte context budget; no request is sent"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{user_request, ChatBackend};

    struct NoopBackend;

    impl ChatBackend for NoopBackend {
        fn request(
            &self,
            _requests: &[serdes_ai::core::ModelRequest],
            _tools: &[crate::tools::ToolSpec],
        ) -> Result<LlmResult, String> {
            unreachable!("request-budget fixtures never call the backend")
        }
    }

    fn materialized_request_fixture(
        prompt: &str,
    ) -> (
        Vec<serdes_ai::core::ModelRequest>,
        Vec<crate::tools::ToolSpec>,
    ) {
        let agent = crate::agent::CodingAgent::with_backend(
            Box::new(NoopBackend),
            std::env::temp_dir(),
            false,
        );
        let reserved = crate::session::ReservedRequest {
            branch_id: "b1".into(),
            turn: 1,
            attempt: 1,
            replay: false,
            retry: false,
            rounds: Vec::new(),
            summary: String::new(),
            prompt: prompt.into(),
            history: Vec::new(),
            owner: "owner".into(),
        };
        (
            agent.materialize_requests(&reserved),
            crate::tools::tool_specs(false),
        )
    }

    /// The complete-request preflight includes the fixed overhead (model id, route,
    /// provider settings, framing) so a request of only part bytes can still trip the
    /// conservative budget gate; it is refused before any backend call.
    #[test]
    fn estimate_request_bytes_includes_fixed_overhead() {
        let (reqs, tools) = materialized_request_fixture("a");
        let est = estimate_request_bytes(&reqs);
        assert!(est > 256);
        assert!(
            round_budget_exceeded(&reqs, &tools, Some(10)),
            "a tiny context budget must refuse the complete request"
        );
        assert!(
            !round_budget_exceeded(&reqs, &tools, None),
            "the default 32MiB budget still accepts a one-prompt request"
        );
        assert_eq!(REQUEST_FIXED_OVERHEAD_BYTES, 8192);
        assert_eq!(
            REQUEST_FIXED_OVERHEAD_BYTES,
            crate::redact::MAX_ERROR_TEXT_BYTES
        );
        assert_eq!(crate::redact::TRUNCATION_MARKER, "[truncated]");
    }

    #[test]
    fn exact_materialized_request_budget_is_inclusive() {
        let mut prompt = String::from("boundary");
        let (requests, tools, total) = loop {
            let (requests, tools) = materialized_request_fixture(&prompt);
            let total = estimate_request_bytes(&requests)
                .saturating_add(crate::adapter::estimate_tool_schema_bytes(&tools));
            if total.checked_rem(3) == Some(0) {
                break (requests, tools, total);
            }
            prompt.push('x');
        };
        let exact_tokens = u64::try_from(total / 3).unwrap();
        assert!(!round_budget_exceeded(
            &requests,
            &tools,
            Some(exact_tokens)
        ));
        assert!(round_budget_exceeded(
            &requests,
            &tools,
            Some(exact_tokens - 1)
        ));
    }

    #[test]
    fn omitting_the_system_prompt_changes_a_budget_decision() {
        let (requests, tools) = materialized_request_fixture("a");
        let incomplete = vec![user_request("a")];
        let incomplete_total = estimate_request_bytes(&incomplete)
            .saturating_add(crate::adapter::estimate_tool_schema_bytes(&tools));
        let fixture_tokens = u64::try_from(incomplete_total.div_ceil(3)).unwrap();
        assert!(!round_budget_exceeded(
            &incomplete,
            &tools,
            Some(fixture_tokens)
        ));
        assert!(round_budget_exceeded(
            &requests,
            &tools,
            Some(fixture_tokens)
        ));
    }

    /// A giant prompt request stays over the complete-request budget: the saturated
    /// estimate cannot wrap to a smaller total, so an over-cap complete request is refused
    /// before any backend call. (A single small request stays *under* the default 32 MiB
    /// budget regardless of the 8192-byte fixed overhead.)
    #[test]
    fn estimate_request_bytes_saturates_and_refuses_oversized() {
        let (reqs, tools) = materialized_request_fixture(&"x".repeat(16 * 1024 * 1024));
        assert!(estimate_request_bytes(&reqs) > 16 * 1024 * 1024);
        assert!(
            round_budget_exceeded(&reqs, &tools, Some(7)),
            "the configured 7-token context-limit (a 21-byte budget) has a tiny byte budget and so refuses a 16 MiB request"
        );
        let (tiny, tiny_tools) = materialized_request_fixture("a");
        let _ = estimate_request_bytes(&tiny);
        assert!(
            !round_budget_exceeded(&tiny, &tiny_tools, None),
            "a single small request stays under the default budget"
        );
    }

    #[test]
    fn oversized_history_is_outside_the_budget() {
        let huge = crate::session::HistoryTurn {
            turn: 1,
            attempt: 1,
            branch_id: "b1".into(),
            prompt: "x".repeat(1_000),
            rounds: vec![],
            summary: String::new(),
        };
        assert!(history_needs_check(std::slice::from_ref(&huge)));
        assert!(!history_within(std::slice::from_ref(&huge), 10));
        assert!(history_within(&[huge], 10 * 1024 * 1024));
        assert!(!history_needs_check(&[]));
    }

    #[test]
    fn model_response_cap_is_a_positive_bound() {
        assert_ne!(MAX_RESPONSE_BYTES, 0);
    }
}
