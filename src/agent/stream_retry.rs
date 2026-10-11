//! Host-owned transport reissues. Failed/partial responses never reach tools or history.
use super::*;

impl Turn<'_> {
    pub(super) fn retry_stream(
        &self,
        requests: &[serdes_ai::core::ModelRequest],
        tools: &[crate::tools::ToolSpec],
        context: (&SessionStore, &ReservedRequest, usize),
    ) -> Result<LlmResult, RoundFailure> {
        let (store, reserved, round) = context;
        if self.telemetry_round.replace(round) != round {
            self.telemetry_attempt.set(0);
        }
        for retry in 0..=super::rate_limit::MAX_RETRIES {
            let result = self.round_once(requests, tools);
            self.persist_observations(store, reserved)?;
            let events = match result {
                Err(RoundFailure::StreamInterrupted(events))
                    if retry < super::rate_limit::MAX_RETRIES =>
                {
                    events
                }
                Err(RoundFailure::ModelTransport(failure))
                    if failure.kind == crate::transport::TransportKind::RateLimit
                        && failure.status == Some(429) =>
                {
                    let started = std::time::Instant::now();
                    let result = self.rate_limit_delay(store, reserved, failure.clone(), retry);
                    if !matches!(result, Err(RoundFailure::RateLimitStopped { .. })) {
                        self.observed_retry(Some(&failure), started.elapsed());
                        self.persist_observations(store, reserved)?;
                    }
                    result?;
                    continue;
                }
                other => return other,
            };
            let record = crate::session::StreamRetryRecord {
                round,
                retry: u8::try_from(retry + 1).expect("retry cap fits u8"),
                delay_ms: 250 * (1 << retry),
                events,
            };
            // Persistence is required even with live emission disabled. Never store
            // provider prose or partial text/args. Renew the existing owner lease.
            store
                .renew_lease(reserved)
                .map_err(RoundFailure::RetryStore)?;
            store
                .record_stream_retry(reserved, record.clone())
                .map_err(RoundFailure::RetryStore)?;
            self.emitter
                .lock()
                .expect("transcript mutex poisoned")
                .stream_retry(reserved.turn, &record)
                .map_err(RoundFailure::RetryTranscript)?;
            let started = std::time::Instant::now();
            let result = self.retry_delay(std::time::Duration::from_millis(record.delay_ms));
            self.observed_retry(None, started.elapsed());
            self.persist_observations(store, reserved)?;
            result?;
        }
        unreachable!("last attempt returns its result")
    }
}

#[cfg(test)]
mod tests;
