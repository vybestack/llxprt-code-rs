//! The coding-agent loop: system prompt, materialized branch history, tool-call
//! execution, and the bounded read/edit/test cycle. Each process `run` drives one turn
//! and persists a transcript for it. The llxprt profile key stays in the adapter
//! and is never logged or persisted.
//!
//! Before every model call the selected branch's history is materialized: the system
//! prompt, each prior user prompt, the assistant text and its tool calls with preserved
//! ids and raw args, the matching tool returns, and the final assistant response. The
//! current turn's prompt follows. Sibling branches are never mixed in (the store only
//! returns the selected lineage).
//!
//! The loop inspects `finish_reason` after every round. Only allowed completion
//! reasons succeed; `length`/`content_filter`/`error`/unknown terminally fail and
//! are persisted. Empty/duplicate ids, unknown or disabled tools, non-object
//! arguments, and the exact tool-call budget are all validated before any side effect.
//! Malformed argument JSON is a hard error, not a normalized `{}` that could execute.

use crate::adapter::{
    assistant_request, make_adapter, persisted_round_requests, system_request, tool_return_request,
    user_request, ChatBackend, LlmResult, ToolCall,
};
use crate::model::ModelConfig;
use crate::session::{ReservedRequest, RoundRecord, SessionStore};
use serde_json::Value as JsonValue;

mod finish;
pub use finish::finish_check;

// Compatibility alias retained while route construction remains owned by the adapter.
pub use crate::adapter::chat_route;

/// Bounded framing overhead (bytes) folded into the conservative preflight estimate for
/// the **complete** outgoing request on top of the message parts. The value stays
/// published from the root module ([`crate::agent`]) for phase 1 consumers.
pub use request_budget::REQUEST_FIXED_OVERHEAD_BYTES;
pub use request_budget::{
    context_exceeded_message, estimate_history_bytes, estimate_request_bytes, history_needs_check,
    history_within, materialization_budget, round_budget_exceeded, turn_args_bytes,
    MAX_RESPONSE_BYTES, MAX_TOOL_CALLS_PER_TURN, MAX_TOOL_CALL_ID_BYTES, MAX_TOOL_NAME_BYTES,
    MAX_TURN_ARGS_BYTES, MAX_TURN_ASSISTANT_BYTES, MAX_TURN_OUTPUT_BYTES, MAX_TURN_ROUNDS,
    PER_PART_OVERHEAD_BYTES, PER_REQUEST_OVERHEAD_BYTES,
};

mod request_budget;

/// Result of a completed turn (either live or replayed).
#[derive(Debug, Clone)]
pub struct CompletedRun {
    pub turn: u32,
    pub attempt: u32,
    pub branch_id: String,
    pub summary: String,
    pub tool_count: usize,
    pub prompt_digest: String,
    pub status: String,
    pub branch: bool,
    pub replayed: bool,
}

/// The headless agent: drives the turn loop using a [`ChatBackend`].
pub struct CodingAgent {
    backend: std::sync::Arc<dyn ChatBackend>,
    cwd: std::path::PathBuf,
    workspace: crate::tools::WorkspaceCap,
    /// The resolved per-prompt tool-call budget: `None` = unlimited, `Some(n)` =
    /// `n` calls. Constructors start at the 16-call default; the CLI resolves
    /// CLI-over-profile and overrides via [`Self::with_max_tool_calls`].
    pub max_tool_calls: Option<usize>,

    /// Wall-clock budget for one prompt turn. `None` (the default) means no
    /// time limit; set from the CLI `--turn-time` flag.
    turn_time_budget: Option<std::time::Duration>,
    max_rounds: usize,
    allow_shell: bool,
    secrets: Vec<String>,
    /// Profile prompt-notes passed to the system prompt (reasoning effort wording).
    pub prompt_notes: Option<String>,
    /// The profile's estimated context budget for materialized history.
    pub context_limit: Option<u64>,
}

mod error;
pub use error::AgentError;
mod helpers;
use helpers::{final_summary_request, tool_call_record, validate_provider_result};
mod config;
pub use config::{
    coding_system_prompt, prompt_digest, round_limit_message, validate_timeout,
    TIMEOUT_LEASE_MARGIN_SECONDS,
};

struct TurnUsage {
    assistant_bytes: usize,
    args_bytes: usize,
    output_bytes: usize,
    total_calls: usize,
}

struct AttemptState {
    requests: Vec<serdes_ai::core::ModelRequest>,
    rounds: Vec<RoundRecord>,
    current: LlmResult,
    ids: std::collections::HashSet<String>,
    usage: TurnUsage,
    started: std::time::Instant,
}

impl CodingAgent {
    /// Build an agent over the real SerdesAI adapter. The timeout is validated against the
    /// session lease first: a single request must always fit inside one lease, so a value at or
    /// above the lease minus the margin is rejected here rather than risk a stale lease
    /// mid-request.
    pub fn new(
        config: &ModelConfig,
        cwd: &std::path::Path,
        allow_shell: bool,
    ) -> Result<CodingAgent, crate::adapter::ModelErrorAdapter> {
        validate_timeout(config.timeout).map_err(|m| crate::adapter::ModelErrorAdapter {
            key: "request-timeout",
            message: m,
            code: crate::cli::Code::Config,
        })?;
        let workspace = crate::tools::WorkspaceCap::open(cwd).map_err(|message| {
            crate::adapter::ModelErrorAdapter {
                key: "workspace",
                message,
                code: crate::cli::Code::Config,
            }
        })?;
        let adapter = make_adapter(config)?;
        Ok(CodingAgent {
            backend: std::sync::Arc::new(adapter),
            cwd: cwd.to_path_buf(),
            workspace,
            max_tool_calls: Some(MAX_TOOL_CALLS_PER_TURN),
            turn_time_budget: None,
            max_rounds: MAX_TURN_ROUNDS,
            allow_shell,
            secrets: config.secret_values(),
            prompt_notes: None,
            context_limit: config.context_limit,
        })
    }

    pub(crate) fn new_with_backend(
        backend: Box<dyn ChatBackend>,
        cwd: &std::path::Path,
        allow_shell: bool,
    ) -> Result<CodingAgent, crate::adapter::ModelErrorAdapter> {
        let workspace = crate::tools::WorkspaceCap::open(cwd).map_err(|message| {
            crate::adapter::ModelErrorAdapter {
                key: "workspace",
                message,
                code: crate::cli::Code::Config,
            }
        })?;
        Ok(CodingAgent {
            backend: std::sync::Arc::from(backend),
            cwd: cwd.to_path_buf(),
            workspace,
            max_tool_calls: Some(MAX_TOOL_CALLS_PER_TURN),
            turn_time_budget: None,
            max_rounds: MAX_TURN_ROUNDS,
            allow_shell,
            secrets: Vec::new(),
            prompt_notes: None,
            context_limit: None,
        })
    }

    /// Build an agent over an arbitrary backend (used by tests with a mock).
    pub fn with_backend(
        backend: Box<dyn ChatBackend>,
        cwd: std::path::PathBuf,
        allow_shell: bool,
    ) -> CodingAgent {
        let workspace = crate::tools::WorkspaceCap::open(&cwd)
            .expect("test backend workspace must be an existing directory");
        CodingAgent {
            backend: std::sync::Arc::from(backend),
            cwd,
            workspace,
            max_tool_calls: Some(MAX_TOOL_CALLS_PER_TURN),
            turn_time_budget: None,
            max_rounds: MAX_TURN_ROUNDS,
            allow_shell,
            secrets: Vec::new(),
            prompt_notes: None,
            context_limit: None,
        }
    }

    /// Override the agent's conservative per-request context budget (tests drive budget
    /// enforcement with an explicit token budget instead of a profile).
    pub fn with_context_limit(mut self, context_limit: Option<u64>) -> CodingAgent {
        self.context_limit = context_limit;
        self
    }

    /// Override the per-turn round cap (tests drive round-cap enforcement with explicit
    /// tool-less budgets instead of the full [`MAX_TURN_ROUNDS`] default).
    pub fn with_max_rounds(mut self, max_rounds: usize) -> CodingAgent {
        self.max_rounds = max_rounds;
        self
    }

    /// Override the resolved per-prompt tool-call budget (`None` = unlimited).
    pub fn with_max_tool_calls(mut self, max_tool_calls: Option<usize>) -> CodingAgent {
        self.max_tool_calls = max_tool_calls;
        self
    }

    /// Override the wall-clock turn budget (`None` = no time limit).
    pub fn with_turn_time(mut self, budget: Option<std::time::Duration>) -> CodingAgent {
        self.turn_time_budget = budget;
        self
    }

    /// Attach explicit secret values this agent must scrub from provider error text before
    /// any CLI output, stderr, or session persistence. The values live only on this agent; there
    /// is no process-global secret state, so nothing can leak across requests or tests.
    pub fn with_secrets(mut self, secrets: Vec<String>) -> CodingAgent {
        self.secrets = secrets;
        self
    }

    /// Number of backend model calls this agent has made so far.
    pub fn model_calls(&self) -> usize {
        self.backend.request_calls()
    }

    /// Drive the reserved branch and persist the final result. A `replay` reservation
    /// never talks to the backend. A `retry` reservation re-runs a previously failed
    /// prompt as a fresh attempt; it never reports ok on its own.
    pub fn run(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
    ) -> Result<CompletedRun, AgentError> {
        store
            .verify_workspace_identity(self.workspace.identity())
            .map_err(AgentError::from_store)?;
        if reserved.replay {
            return Ok(Self::replayed_run(reserved));
        }
        store
            .renew_lease(reserved)
            .map_err(AgentError::from_store)?;
        self.validate_history_budget(store, reserved)?;
        let requests = self.materialize_requests(reserved);
        let tools = crate::tools::tool_specs(self.allow_shell);
        let config = self
            .tools_config(self.allow_shell)
            .map_err(|error| self.dead(store, reserved, "workspace", &error, &[]))?;
        let mut attempt = self.begin_attempt(store, reserved, requests, &tools)?;
        self.run_tool_rounds(store, reserved, &tools, &config, &mut attempt)?;
        let summary = self.resolve_summary(store, reserved, &tools, &mut attempt)?;
        self.complete_attempt(store, reserved, summary, attempt)
    }

    fn replayed_run(reserved: &ReservedRequest) -> CompletedRun {
        CompletedRun {
            turn: reserved.turn,
            attempt: reserved.attempt,
            branch_id: reserved.branch_id.clone(),
            summary: reserved.summary.clone(),
            tool_count: reserved.rounds.iter().map(|round| round.calls.len()).sum(),
            prompt_digest: prompt_digest(&reserved.prompt),
            status: "ok".into(),
            branch: reserved.attempt > 1,
            replayed: true,
        }
    }

    fn validate_history_budget(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
    ) -> Result<(), AgentError> {
        let needs_check = history_needs_check(&reserved.history);
        let within = history_within(
            &reserved.history,
            materialization_budget(self.context_limit),
        );
        if needs_check && !within {
            return Err(self.dead(
                store,
                reserved,
                "context-limit",
                "materialized history would exceed the profile context budget",
                &[],
            ));
        }
        Ok(())
    }

    fn materialize_requests(
        &self,
        reserved: &ReservedRequest,
    ) -> Vec<serdes_ai::core::ModelRequest> {
        let note = self.prompt_notes.as_deref().unwrap_or("");
        let mut requests = vec![system_request(&coding_system_prompt(
            &self.cwd,
            note,
            self.allow_shell,
            self.max_tool_calls,
        ))];
        for history in &reserved.history {
            requests.push(user_request(&history.prompt));
            for round in &history.rounds {
                requests.extend(persisted_round_requests(round));
            }
        }
        requests.push(user_request(&reserved.prompt));
        requests
    }

    fn begin_attempt(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        requests: Vec<serdes_ai::core::ModelRequest>,
        tools: &[crate::tools::ToolSpec],
    ) -> Result<AttemptState, AgentError> {
        self.renew(store, reserved)?;
        self.check_request_budget(store, reserved, &requests, tools, &[])?;
        let current = self
            .round(&requests, tools)
            .map_err(|error| self.dead(store, reserved, "model", &error, &[]))?;
        self.renew(store, reserved)?;
        self.check_finish(store, reserved, &current, &[])?;
        let usage = TurnUsage {
            assistant_bytes: current.text.len(),
            args_bytes: turn_args_bytes(&current),
            output_bytes: 0,
            total_calls: 0,
        };
        self.enforce_usage(store, reserved, &[], &usage)?;
        Ok(AttemptState {
            requests,
            rounds: Vec::new(),
            current,
            ids: std::collections::HashSet::new(),
            usage,
            started: std::time::Instant::now(),
        })
    }

    fn run_tool_rounds(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        tools: &[crate::tools::ToolSpec],
        config: &crate::tools::ToolConfig,
        attempt: &mut AttemptState,
    ) -> Result<(), AgentError> {
        while !attempt.current.calls.is_empty() {
            self.execute_tool_round(store, reserved, config, attempt)?;
            self.request_next_round(store, reserved, tools, attempt)?;
        }
        Ok(())
    }

    fn execute_tool_round(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        config: &crate::tools::ToolConfig,
        attempt: &mut AttemptState,
    ) -> Result<(), AgentError> {
        self.check_round_limit(store, reserved, &attempt.rounds)?;
        self.check_time_limit(store, reserved, &attempt.rounds, attempt.started.elapsed())?;
        let calls = validate_calls(&mut attempt.ids, &attempt.current, self.allow_shell).map_err(
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
        self.check_tool_limit(
            store,
            reserved,
            &attempt.rounds,
            &attempt.usage,
            calls.len(),
        )?;
        attempt.requests.push(assistant_request(&attempt.current));
        let mut round = RoundRecord {
            assistant: attempt.current.text.clone(),
            calls: Vec::new(),
        };
        for (index, call) in calls.iter().enumerate() {
            self.execute_one_call(config, attempt, &mut round, call, index, calls.len())
                .map_err(|failure| match failure {
                    ToolCallFailure::Invalid(error) => self.dead(
                        store,
                        reserved,
                        "invalid-tool-call",
                        &error,
                        &attempt.rounds,
                    ),
                    ToolCallFailure::OutputCap => self.dead(
                        store,
                        reserved,
                        "limit",
                        &format!("turn tool output reached the {MAX_TURN_OUTPUT_BYTES} byte cap"),
                        &attempt.rounds,
                    ),
                })?;
        }
        attempt.rounds.push(round);
        self.enforce_usage(store, reserved, &attempt.rounds, &attempt.usage)?;
        store
            .checkpoint(reserved, &attempt.rounds)
            .map_err(AgentError::from_store)
    }

    /// Execute one tool call of a round and record it. `index`/`total` decide
    /// whether the budget notice rides on this result (last call of the round).
    /// Failures carry their kind so the caller keeps the right error code.
    fn execute_one_call(
        &self,
        config: &crate::tools::ToolConfig,
        attempt: &mut AttemptState,
        round: &mut RoundRecord,
        call: &ToolCall,
        index: usize,
        total: usize,
    ) -> Result<(), ToolCallFailure> {
        let remaining_output = MAX_TURN_OUTPUT_BYTES.saturating_sub(attempt.usage.output_bytes);
        if remaining_output == 0 {
            return Err(ToolCallFailure::OutputCap);
        }
        let parsed = parse_object_args(call).map_err(ToolCallFailure::Invalid)?;
        let (ok, raw_text) = crate::tools::execute_tool_with_limit(
            &self.cwd,
            &call.name,
            parsed,
            config,
            remaining_output,
        );
        let scrubbed = crate::redact::scrub_secrets(&raw_text, &self.secrets);
        let text = crate::redact::truncate_utf8(scrubbed, remaining_output);
        attempt.usage.total_calls += 1;
        let text = if index + 1 == total {
            let notice = budget_notice(self.max_tool_calls, attempt.usage.total_calls);
            if notice.is_empty() {
                text
            } else {
                format!("{text}\n\n{notice}")
            }
        } else {
            text
        };
        attempt.usage.output_bytes = attempt.usage.output_bytes.saturating_add(text.len());
        attempt
            .requests
            .push(tool_return_request(&call.name, &call.id, ok, &text));
        round.calls.push(tool_call_record(call, ok, text));
        Ok(())
    }

    fn request_next_round(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        tools: &[crate::tools::ToolSpec],
        attempt: &mut AttemptState,
    ) -> Result<(), AgentError> {
        self.check_request_budget(store, reserved, &attempt.requests, tools, &attempt.rounds)?;
        self.renew(store, reserved)?;
        attempt.current = self
            .round(&attempt.requests, tools)
            .map_err(|error| self.dead(store, reserved, "model", &error, &attempt.rounds))?;
        self.renew(store, reserved)?;
        attempt.usage.assistant_bytes = attempt
            .usage
            .assistant_bytes
            .saturating_add(attempt.current.text.len());
        attempt.usage.args_bytes = attempt
            .usage
            .args_bytes
            .saturating_add(turn_args_bytes(&attempt.current));
        self.enforce_usage(store, reserved, &attempt.rounds, &attempt.usage)?;
        self.check_finish(store, reserved, &attempt.current, &attempt.rounds)
    }

    fn check_request_budget(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        requests: &[serdes_ai::core::ModelRequest],
        tools: &[crate::tools::ToolSpec],
        rounds: &[RoundRecord],
    ) -> Result<(), AgentError> {
        if round_budget_exceeded(requests, tools, self.context_limit) {
            return Err(self.dead(
                store,
                reserved,
                "context-limit",
                &context_exceeded_message(requests, tools, self.context_limit),
                rounds,
            ));
        }
        Ok(())
    }

    fn check_round_limit(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        rounds: &[RoundRecord],
    ) -> Result<(), AgentError> {
        if rounds.len() + 1 > self.max_rounds {
            return Err(self.dead(
                store,
                reserved,
                "turn-budget",
                &round_limit_message(self.max_rounds),
                rounds,
            ));
        }
        Ok(())
    }

    fn check_tool_limit(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        rounds: &[RoundRecord],
        usage: &TurnUsage,
        next: usize,
    ) -> Result<(), AgentError> {
        let Some(max) = self.max_tool_calls else {
            return Ok(());
        };
        if usage.total_calls + next > max {
            return Err(self.dead(
                store,
                reserved,
                "budget-exhausted",
                &format!("tool-call budget ({max}) would be exceeded; reduce tool calls"),
                rounds,
            ));
        }
        Ok(())
    }

    /// Enforce the wall-clock turn budget at the top of every tool round,
    /// alongside the round and tool-call limits, so a turn that outlives its
    /// budget fails in the same envelope instead of hanging.
    fn check_time_limit(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        rounds: &[RoundRecord],
        elapsed: std::time::Duration,
    ) -> Result<(), AgentError> {
        let Some(budget) = self.turn_time_budget else {
            return Ok(());
        };
        if elapsed >= budget {
            return Err(self.dead(
                store,
                reserved,
                "turn-time-exhausted",
                &format!(
                    "turn time budget ({}s) exceeded; raise --turn-time or pass 0 to disable",
                    budget.as_secs()
                ),
                rounds,
            ));
        }
        Ok(())
    }

    fn enforce_usage(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        rounds: &[RoundRecord],
        usage: &TurnUsage,
    ) -> Result<(), AgentError> {
        self.enforce_turn_caps(
            usage.assistant_bytes,
            usage.args_bytes,
            usage.output_bytes,
            store,
            reserved,
            rounds,
        )
    }

    fn resolve_summary(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        tools: &[crate::tools::ToolSpec],
        attempt: &mut AttemptState,
    ) -> Result<String, AgentError> {
        if !attempt.current.text.trim().is_empty() {
            self.check_round_limit(store, reserved, &attempt.rounds)?;
            return Ok(std::mem::take(&mut attempt.current.text));
        }
        self.forced_summary(store, reserved, tools, attempt)
    }

    fn forced_summary(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        tools: &[crate::tools::ToolSpec],
        attempt: &mut AttemptState,
    ) -> Result<String, AgentError> {
        self.enforce_usage(store, reserved, &attempt.rounds, &attempt.usage)?;
        attempt.requests.push(assistant_request(&attempt.current));
        attempt.requests.push(final_summary_request());
        self.check_request_budget(store, reserved, &attempt.requests, tools, &attempt.rounds)?;
        let forced =
            self.run_final_round(store, reserved, &attempt.requests, tools, &attempt.rounds)?;
        self.validate_forced_summary(store, reserved, &mut attempt.ids, &attempt.rounds, &forced)?;
        attempt.usage.assistant_bytes = attempt
            .usage
            .assistant_bytes
            .saturating_add(forced.text.len());
        self.enforce_usage(store, reserved, &attempt.rounds, &attempt.usage)?;
        self.check_round_limit(store, reserved, &attempt.rounds)?;
        Ok(forced.text)
    }

    fn validate_forced_summary(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        ids: &mut std::collections::HashSet<String>,
        rounds: &[RoundRecord],
        forced: &LlmResult,
    ) -> Result<(), AgentError> {
        let calls = validate_calls(ids, forced, self.allow_shell)
            .map_err(|error| self.dead(store, reserved, "invalid-tool-call", &error, rounds))?;
        if !calls.is_empty() {
            return Err(self.dead(
                store,
                reserved,
                "invalid-tool-call",
                "final summary round asked for tools again; giving up",
                rounds,
            ));
        }
        finish_check(forced)
            .map_err(|error| self.dead(store, reserved, "finish-reason", &error, rounds))?;
        if forced.text.trim().is_empty() {
            return Err(self.dead(
                store,
                reserved,
                "empty-final-output",
                "no summary text from the model",
                rounds,
            ));
        }
        Ok(())
    }

    fn complete_attempt(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        summary: String,
        mut attempt: AttemptState,
    ) -> Result<CompletedRun, AgentError> {
        attempt.rounds.push(RoundRecord {
            assistant: summary.clone(),
            calls: Vec::new(),
        });
        store
            .finalize(reserved, &summary, &attempt.rounds)
            .map_err(AgentError::from_store)?;
        Ok(CompletedRun {
            turn: reserved.turn,
            attempt: reserved.attempt,
            branch_id: reserved.branch_id.clone(),
            summary,
            tool_count: attempt.usage.total_calls,
            prompt_digest: prompt_digest(&reserved.prompt),
            status: "ok".into(),
            branch: reserved.attempt > 1,
            replayed: false,
        })
    }

    /// Reason-effort (or other request-side) profile notes to append to the system
    /// prompt. This is a text note about the author's intent; the transport never
    /// forwards a reasoning field. The note is bounded (a profile value is capped at
    /// [`crate::redact::MAX_PROMPT_NOTE_BYTES`] and the accumulated prompt text
    /// carries its own documented cap in [`crate::redact::PROMPT_NOTE_CAP_MESSAGE`]).
    pub fn prompt_reason_note(profile: &crate::profile::Profile) -> Option<String> {
        let s = profile
            .ephemeral
            .prompt_notes
            .get("reasoning:reasoning.effort")?;
        let note = format!("reasoning effort requested by profile (prompt note only): {s}");
        if note.len() > crate::redact::MAX_PROMPT_NOTE_BYTES {
            return Some(crate::redact::PROMPT_NOTE_CAP_MESSAGE.to_string());
        }
        Some(note)
    }

    /// The `finished` round, e.g. the final summary request, is also a model request: renew
    /// around it too.
    fn renew(&self, store: &SessionStore, reserved: &ReservedRequest) -> Result<(), AgentError> {
        store.renew_lease(reserved).map_err(AgentError::from_store)
    }

    fn check_finish(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        result: &LlmResult,
        rounds: &[RoundRecord],
    ) -> Result<(), AgentError> {
        match finish_check(result) {
            Ok(()) => Ok(()),
            Err(e) => Err(self.dead(store, reserved, "finish-reason", &e, rounds)),
        }
    }

    /// Persist a terminal failure and surface it as an [`AgentError`]. The message is
    /// scrubbed first (every accepted secret and credential path), then bounded to
    /// [`crate::redact::MAX_ERROR_TEXT_BYTES`] at a UTF-8 boundary with the
    /// explicit `[truncated]` marker; that bounded scrubbed text is what the
    /// session fail and the CLI JSON both receive, so a huge provider body must leave a
    /// terminal failed lifecycle and retain the model exit code, never become
    /// session-persist. A persistence failure is never discarded: it becomes a
    /// session error that still carries the original scrubbed bounded message.
    fn dead(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        key: &'static str,
        message: &str,
        rounds: &[RoundRecord],
    ) -> AgentError {
        let bounded = crate::redact::scrub_and_bound(message, &self.secrets);
        match store.fail(reserved, &bounded, rounds) {
            Ok(()) => AgentError::new(crate::cli::Code::Model, key, bounded),
            Err(pe) => AgentError::new(
                crate::cli::Code::Session,
                "session-persist",
                format!(
                    "turn failed ({key}: {bounded}); additionally, persisting the failure failed: {pe}"
                ),
            ),
        }
    }

    /// Enforce the aggregate per-turn caps for the whole attempt so far: combined assistant
    /// text (including a forced final summary), raw tool-call arguments, and tool-output
    /// bytes. Checked before every checkpoint and before every model call, and the forced
    /// response is folded into the assistant total and re-asserted before the finalize, so
    /// the cumulative sizes across every round (ordinary and final) all count, not just one
    /// oversized individual round. An over-cap attempt is a typed terminal failure. The
    /// round cap is enforced separately at the top of the loop, before any side effect of
    /// the next round runs.
    fn enforce_turn_caps(
        &self,
        assistant_bytes: usize,
        args_bytes: usize,
        output_bytes: usize,
        store: &SessionStore,
        reserved: &ReservedRequest,
        rounds: &[RoundRecord],
    ) -> Result<(), AgentError> {
        if assistant_bytes > MAX_TURN_ASSISTANT_BYTES {
            return Err(self.dead(
                store,
                reserved,
                "turn-budget",
                &format!(
                    "turn assistant content would exceed the {MAX_TURN_ASSISTANT_BYTES} byte cap",
                ),
                rounds,
            ));
        }
        if args_bytes > MAX_TURN_ARGS_BYTES {
            return Err(self.dead(
                store,
                reserved,
                "turn-budget",
                &format!(
                    "turn tool-call arguments would exceed the {MAX_TURN_ARGS_BYTES} byte cap",
                ),
                rounds,
            ));
        }
        if output_bytes > MAX_TURN_OUTPUT_BYTES {
            return Err(self.dead(
                store,
                reserved,
                "turn-budget",
                &format!("turn tool output would exceed the {MAX_TURN_OUTPUT_BYTES} byte cap",),
                rounds,
            ));
        }
        Ok(())
    }

    /// Run the forced final-summary round, renewing the lease around the model request. The
    /// renewal before the call extends the lease so the request can always finish inside one
    /// lease, and the renewal after the call re-arms it for the persist; the lease is never
    /// left held dead by the bounded max-`MAX_TURN_ROUNDS`-round finalize.
    fn run_final_round(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        requests: &[serdes_ai::core::ModelRequest],
        tools: &[crate::tools::ToolSpec],
        persisted_rounds: &[RoundRecord],
    ) -> Result<LlmResult, AgentError> {
        self.renew(store, reserved)?;
        let r = self
            .round(requests, tools)
            .map_err(|e| self.dead(store, reserved, "model", &e, persisted_rounds))?;
        self.renew(store, reserved)?;
        Ok(r)
    }

    fn round(
        &self,
        requests: &[serdes_ai::core::ModelRequest],
        tools: &[crate::tools::ToolSpec],
    ) -> Result<LlmResult, String> {
        // A single oversized model reply is bounded before it is any round: a reply over
        // [`MAX_RESPONSE_BYTES`] is a typed model failure here, so an unbounded
        // provider body can never be accumulated (or double-counted) into the aggregate
        // assistant bytes or become session-persist. The error carries the round's own
        // size, which stays bounded by the reply cap.
        let result = self.backend.request(requests, tools)?;
        validate_provider_result(&result, &self.secrets)?;
        Ok(result)
    }

    fn tools_config(&self, shell_on: bool) -> Result<crate::tools::ToolConfig, String> {
        Ok(crate::tools::ToolConfig {
            ws: self.workspace.try_clone()?,
            max_output_bytes: crate::tools::output_limits::MAX_TOOL_OUTPUT_DEFAULT,
            shell: crate::tools::ShellConfig {
                max_shell_output: crate::tools::output_limits::MAX_SHELL_OUTPUT_DEFAULT,
                max_shell_timeout: std::time::Duration::from_secs(120),
                allow_shell: shell_on,
            },
        })
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

/// whole attempt (`seen`), a known *and enabled* tool name, and a JSON object of arguments.
fn validate_calls(
    seen: &mut std::collections::HashSet<String>,
    result: &LlmResult,
    allow_shell: bool,
) -> Result<Vec<ToolCall>, String> {
    for c in &result.calls {
        if c.id.trim().is_empty() {
            return Err("model returned a tool call with an empty id".into());
        }
        if !seen.insert(c.id.clone()) {
            return Err(format!("duplicate tool call id {}", c.id));
        }
    }
    for c in &result.calls {
        match serde_json::from_str::<serde_json::Value>(&c.args_json) {
            Ok(serde_json::Value::Object(_)) => {}
            Ok(_) => {
                return Err(format!(
                    "tool call {}: arguments must be a JSON object",
                    c.name
                ));
            }
            Err(e) => return Err(format!("tool call {}: invalid argument JSON: {e}", c.name)),
        }
    }
    for c in &result.calls {
        if !known_tool(&c.name, allow_shell) {
            return Err(format!("unknown or disabled tool {}", c.name));
        }
    }
    Ok(result.calls.clone())
}

/// Whether a tool name is known and (for shell) enabled.
pub fn known_tool(name: &str, allow_shell: bool) -> bool {
    matches!(
        name,
        "read_file" | "write_file" | "replace" | "list_directory" | "search_file_content"
    ) || (name == "run_shell_command" && allow_shell)
}

/// The budget notice appended to the last tool result of a round so the model
/// always knows what remains. `None` budget stays silent.
/// Per-call failure kinds that keep the caller's error codes intact.
enum ToolCallFailure {
    Invalid(String),
    OutputCap,
}

fn budget_notice(budget: Option<usize>, used: usize) -> String {
    let Some(max) = budget else {
        return String::new();
    };
    let left = max.saturating_sub(used);
    if left == 0 {
        format!("[budget: 0 of {max} tool calls left; reply with your final summary only]")
    } else if left <= 3 {
        format!("[budget: only {left} of {max} tool calls left; wrap up and produce your final summary]")
    } else {
        format!("[budget: {left} of {max} tool calls left]")
    }
}

#[cfg(test)]
mod tests;
