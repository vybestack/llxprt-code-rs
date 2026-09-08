//! Fixed request retry policy; no transcript or tool dispatch occurs in this loop.

use super::*;
use crate::adapter::ModelFailure;
use crate::envelope::RequestAttempts;
use std::cell::Cell;
use std::hash::{BuildHasher, Hasher};
use std::time::Duration;

pub(super) const MAX_ATTEMPTS: u32 = 4;
const MAX_SLEEP: Duration = Duration::from_secs(30);

/// Equal jitter: half to all of the exponential window (1s, 2s, 4s).
/// A provider hint is a minimum, never an invitation to retry sooner. Hints above
/// our hard sleep bound terminate rather than silently poking the provider early.
fn delay(attempt: u32, retry_after: Option<Duration>, jitter: u64) -> Duration {
    let window_ms = 1_000_u64 << (attempt - 1);
    let half = window_ms / 2;
    Duration::from_millis(half + jitter % (half + 1)).max(retry_after.unwrap_or_default())
}

fn jitter() -> u64 {
    std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
}

impl Turn<'_> {
    pub(super) async fn request_with_retry(
        &self,
        requests: &[serdes_ai::core::ModelRequest],
        tools: &[crate::tools::ToolSpec],
    ) -> Result<LlmResult, RoundFailure> {
        for attempt in 1..=MAX_ATTEMPTS {
            if self.deadline.is_some_and(|end| Instant::now() >= end) {
                return Err(RoundFailure::TurnTime);
            }
            self.store
                .renew_lease(self.reserved)
                .map_err(|error| RoundFailure::Persistence(AgentError::from_store(error)))?;
            if self.deadline.is_some_and(|end| Instant::now() >= end) {
                return Err(RoundFailure::TurnTime);
            }
            count_attempt(&self.request_attempts, attempt > 1);
            let result = self.backend.request(requests, tools).await;
            if self.deadline.is_some_and(|end| Instant::now() >= end) {
                return Err(RoundFailure::TurnTime);
            }
            let failure = match result {
                Ok(reply) => return Ok(reply),
                Err(ModelFailure::Transport(failure)) => failure,
                Err(ModelFailure::Incomplete(failure)) => {
                    return Err(RoundFailure::ModelTransport(failure))
                }
                Err(ModelFailure::ContextLimit(message)) => {
                    return Err(RoundFailure::ContextLimit(message));
                }
                Err(ModelFailure::Terminal(message)) => return Err(RoundFailure::Model(message)),
            };
            if !failure.kind.is_retryable() || attempt == MAX_ATTEMPTS {
                return Err(RoundFailure::ModelTransport(failure));
            }
            let wait = delay(attempt, failure.retry_after, jitter());
            // The enclosing timeout covers both provider attempts and sleep. A hint
            // beyond the finite sleep policy may still wait out a nearer turn deadline.
            let remaining = self
                .deadline
                .map(|end| end.saturating_duration_since(Instant::now()));
            if wait > MAX_SLEEP && remaining.is_none_or(|left| left > MAX_SLEEP) {
                return Err(RoundFailure::ModelTransport(failure));
            }
            tokio::time::sleep(wait.min(MAX_SLEEP)).await;
        }
        unreachable!("every final attempt returns")
    }
}

fn count_attempt(counts: &Cell<RequestAttempts>, retry: bool) {
    let mut value = counts.get();
    value.attempts += 1;
    value.retries += u64::from(retry);
    counts.set(value);
}

#[cfg(test)]
mod tests;
