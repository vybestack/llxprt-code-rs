//! Bounded host policy for retryable HTTP 429s, not quota or status-free failures.
use super::*;
use std::hash::{BuildHasher, Hasher};
use std::time::Duration;

pub(super) const MAX_RETRIES: usize = 3;
const MAX_DELAY: Duration = Duration::from_secs(30);

fn delay(retry: usize, hint: Option<Duration>, entropy: u64) -> Option<Duration> {
    let hint = hint.unwrap_or_default();
    if hint > MAX_DELAY {
        return None;
    }
    // Exponential equal jitter: 250..500, 500..1000, 1000..2000 milliseconds.
    // The provider hint is a minimum, never shortened to fit the host bound.
    let half = 250u64 << retry;
    let jitter = Duration::from_millis(half + entropy % (half + 1));
    Some((hint + jitter).min(MAX_DELAY))
}

impl Turn<'_> {
    pub(super) fn rate_limit_delay(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        failure: TransportFailure,
        retries: usize,
    ) -> Result<(), RoundFailure> {
        if retries == MAX_RETRIES {
            return Err(RoundFailure::RateLimitStopped {
                failure,
                retries,
                reason: "retry cap exhausted",
            });
        }
        // RandomState provides process-local randomized entropy without introducing
        // another RNG dependency or a provider-controlled seed.
        let entropy = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish();
        let Some(wait) = delay(retries, failure.retry_after, entropy) else {
            return Err(RoundFailure::RateLimitStopped {
                failure,
                retries,
                reason: "Retry-After exceeds the 30s wait bound; no early retry sent",
            });
        };
        store
            .renew_lease(reserved)
            .map_err(RoundFailure::RetryStore)?;
        self.retry_delay(wait)
    }
}

#[cfg(test)]
mod tests;
