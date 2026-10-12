//! Instrument the production turn boundaries without changing their decisions.
use super::*;
use crate::telemetry::{millis, ModelCall, Retry, Telemetry};

impl Turn<'_> {
    pub(super) fn observe_tool(
        &self,
        round: usize,
        call: &ToolCall,
        ok: bool,
        output: &str,
        elapsed: std::time::Duration,
    ) {
        self.observations
            .borrow_mut()
            .push_tool(crate::telemetry::ToolExecution {
                round: round as u64,
                name: if known_tool(&call.name, self.allow_shell) {
                    call.name.clone()
                } else {
                    "unregistered".into()
                },
                exit_class: if ok { "ok" } else { "failed" }.into(),
                wall_ms: millis(elapsed),
                output_bytes: output.len() as u64,
                truncated: output.contains(crate::redact::TRUNCATION_MARKER)
                    || output.contains("**truncated**"),
            });
    }

    pub(super) fn observed_round(
        &self,
        requests: &[serdes_ai::core::ModelRequest],
        tools: &[crate::tools::ToolSpec],
        reattempt: bool,
    ) -> Result<LlmResult, RoundFailure> {
        self.telemetry_attempt
            .set(self.telemetry_attempt.get().saturating_add(1));
        let started = std::time::Instant::now();
        let collector = serdes_ai::models::telemetry::Collector::default();
        let mut result = self.round_once_inner(requests, tools, reattempt, &collector);
        let observed = collector.snapshot();
        let wall = started.elapsed();
        if let Ok(reply) = &mut result {
            reply.usage.reasoning_tokens =
                observed.reasoning_tokens.or(reply.usage.reasoning_tokens);
            reply.usage.total_tokens = if self.backend.streams_response() {
                observed.total_tokens
            } else {
                reply.usage.total_tokens
            };
        }
        let mut call = ModelCall {
            round: self.telemetry_round.get() as u64,
            attempt: self.telemetry_attempt.get(),
            request_estimate_bytes: estimate_request_bytes(requests)
                .saturating_add(crate::adapter::estimate_tool_schema_bytes(tools))
                as u64,
            wall_ms: millis(wall),
            ttft_ms: observed.first_body_ms,
            wait_ms: observed.first_body_ms,
            connect_queue_ms: observed.headers_ms,
            ..Default::default()
        };
        call.request_token_estimate = call.request_estimate_bytes.div_ceil(3);
        // Only the streamed Codex route separates event arrival from generation. A JSON
        // body arrives after generation, so claiming a throughput there would be false.
        if self.backend.streams_response() {
            call.gen_ms = observed
                .first_body_ms
                .zip(observed.completed_ms)
                .map(|(first, completed)| completed.saturating_sub(first));
        }
        self.fill_usage(&mut call, &result);
        result = self.validated_reply(result);
        if let Err(failure) = &result {
            call.outcome = outcome(failure).into();
        }
        let mut telemetry = self.observations.borrow_mut();
        if let Some(previous) = telemetry
            .rounds
            .last_mut()
            .filter(|c| c.round == call.round)
        {
            let retry = previous.retry.get_or_insert_with(|| Retry {
                error_class: previous
                    .finish_reason
                    .as_deref()
                    .unwrap_or(&previous.outcome)
                    .into(),
                http_status: None,
                retry_after_ms: None,
                backoff_ms: 0,
                succeeded: false,
            });
            retry.succeeded = result.is_ok();
        }
        telemetry.push_call(call);
        result
    }

    fn validated_reply(
        &self,
        result: Result<LlmResult, RoundFailure>,
    ) -> Result<LlmResult, RoundFailure> {
        let mut reply = result?;
        provider_validation::validate(&reply, &self.secrets)?;
        reply.thinking = crate::redact::truncate_utf8(
            crate::redact::scrub_secrets(&reply.thinking, &self.secrets),
            MAX_TURN_ASSISTANT_BYTES,
        );
        if let (Some(output), Some(snapshot)) =
            (&self.cache_output, self.backend.cache_observation())
        {
            output
                .publish(&snapshot)
                .map_err(RoundFailure::CacheOutput)?;
        }
        Ok(reply)
    }

    fn fill_usage(&self, call: &mut ModelCall, result: &Result<LlmResult, RoundFailure>) {
        match &result {
            Ok(reply) => {
                call.outcome = "completed".into();
                call.reported_input_tokens = reply.usage.request_tokens;
                call.input_tokens = reply.usage.request_tokens;
                call.output_tokens = reply.usage.response_tokens;
                call.total_tokens = reply.usage.total_tokens;
                call.total_tokens_source = if call.total_tokens.is_some() {
                    "provider"
                } else {
                    "unavailable"
                }
                .into();
                call.reasoning_tokens = reply.usage.reasoning_tokens;
                call.cached_input_tokens = reply.usage.cache_read_tokens;
                call.cache_creation_input_tokens = reply.usage.cache_creation_tokens;
                if let Some(snapshot) = self.backend.cache_observation() {
                    call.input_tokens = snapshot.call.total_input_tokens;
                    call.uncached_input_tokens = snapshot.call.uncached_input_tokens;
                    if snapshot.call.input_accounting == "input_excludes_cache_reads_and_creation" {
                        call.total_tokens = call
                            .input_tokens
                            .zip(call.output_tokens)
                            .and_then(|(input, output)| input.checked_add(output));
                        call.total_tokens_source = "derived-normalized".into();
                    }
                } else {
                    call.uncached_input_tokens = call
                        .input_tokens
                        .zip(call.cached_input_tokens)
                        .and_then(|(input, cached)| input.checked_sub(cached));
                }
                call.finish_reason = Some(reason(reply.finish_reason.as_ref()).into());
                call.tok_s = call
                    .output_tokens
                    .zip(call.gen_ms)
                    .filter(|(_, ms)| *ms > 0)
                    .map(|(tokens, ms)| tokens as f64 * 1000.0 / ms as f64);
            }
            Err(failure) => call.outcome = outcome(failure).into(),
        }
    }

    pub(super) fn observed_retry(
        &self,
        failure: Option<&TransportFailure>,
        elapsed: std::time::Duration,
    ) {
        if let Some(call) = self.observations.borrow_mut().rounds.last_mut() {
            call.retry = Some(Retry {
                error_class: failure
                    .map_or("stream-interrupted", |f| f.kind.token())
                    .into(),
                http_status: failure.and_then(|f| f.status),
                retry_after_ms: failure.and_then(|f| f.retry_after).map(millis),
                backoff_ms: millis(elapsed),
                succeeded: false,
            });
        }
    }

    pub(super) fn persist_observations(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
    ) -> Result<(), RoundFailure> {
        store
            .record_telemetry(reserved, &self.snapshot_observations(store))
            .map_err(RoundFailure::RetryStore)
    }

    pub(super) fn snapshot_observations(&self, store: &SessionStore) -> Telemetry {
        let mut telemetry = self.observations.borrow().clone();
        telemetry.turn_wall_ms = millis(self.started.elapsed());
        for call in &mut telemetry.rounds {
            call.tool_calls = telemetry
                .tools
                .iter()
                .filter(|t| t.round == call.round && t.exit_class != "budget-refused")
                .count() as u64;
        }
        store.observe_store(&mut telemetry);
        telemetry.summarize();
        telemetry
    }
}

fn reason(reason: Option<&serdes_ai::core::FinishReason>) -> &'static str {
    use serdes_ai::core::FinishReason::*;
    match reason {
        Some(Stop) => "stop",
        Some(Length) => "length",
        Some(ToolCall) => "tool_call",
        Some(ContentFilter) => "content_filter",
        Some(Error) => "error",
        Some(EndTurn) => "end_turn",
        Some(StopSequence) => "stop_sequence",
        Some(Other(_)) => "other",
        None => "missing",
    }
}

fn outcome(failure: &RoundFailure) -> &'static str {
    match failure {
        RoundFailure::TurnTime => "cancelled",
        RoundFailure::StreamInterrupted(_) => "stream-interrupted",
        RoundFailure::ModelTransport(f)
        | RoundFailure::TransportRetryStopped { failure: f, .. } => f.kind.token(),
        _ => "failed",
    }
}
