//! Host-owned SSE reissues. Partial responses never reach tools or model history.
use super::*;

impl Turn<'_> {
    pub(super) fn retry_stream(
        &self,
        requests: &[serdes_ai::core::ModelRequest],
        tools: &[crate::tools::ToolSpec],
        context: (&SessionStore, &ReservedRequest, usize),
    ) -> Result<LlmResult, RoundFailure> {
        let (store, reserved, round) = context;
        for retry in 0..=3 {
            let events = match self.round_once(requests, tools) {
                Err(RoundFailure::StreamInterrupted(events)) if retry < 3 => events,
                other => return other,
            };
            let record = crate::session::StreamRetryRecord {
                round,
                retry: retry + 1,
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
            self.retry_delay(std::time::Duration::from_millis(record.delay_ms))?;
        }
        unreachable!("last attempt returns its result")
    }
}

#[cfg(test)]
mod tests;
