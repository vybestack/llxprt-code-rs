//! One execution clock and executor for a live turn, shared by every provider call.
//! Setup (including credential lookup and session reservation) precedes this boundary.

use super::*;
use tokio::time::Instant;

/// Longest teardown waits for spawned work after a turn ends. Blocking-pool jobs
/// that cannot be cancelled (OS DNS resolution inside the transport) finish
/// detached past this bound instead of delaying the terminal envelope.
const RUNTIME_SHUTDOWN_BOUND: std::time::Duration = std::time::Duration::from_millis(250);

/// A replay never constructs this state. Dropping a timed-out request cancels its
/// transport before the caller persists failure. Teardown also cancels any reader
/// tasks spawned by the transport on this executor; shutdown is bounded, so
/// blocking-pool work that cannot be cancelled (an OS DNS lookup) finishes
/// detached instead of delaying the terminal envelope.
pub(super) struct Turn<'a> {
    agent: &'a CodingAgent,
    pub(super) started: Instant,
    pub(super) deadline: Option<Instant>,
    pub(super) retry_attempts: std::cell::Cell<u64>,
    pub(super) runtime: Option<tokio::runtime::Runtime>,
}

impl std::ops::Deref for Turn<'_> {
    type Target = CodingAgent;

    fn deref(&self) -> &Self::Target {
        self.agent
    }
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(RUNTIME_SHUTDOWN_BOUND);
        }
    }
}

impl<'a> Turn<'a> {
    pub(super) fn new(agent: &'a CodingAgent) -> Result<Self, AgentError> {
        let started = Instant::now();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| {
                AgentError::new(
                    crate::envelope::Code::Model,
                    "runtime",
                    format!("runtime: {error}"),
                )
            })?;
        let deadline = match agent.turn_time_budget {
            Some(budget) => match started.checked_add(budget) {
                Some(deadline) => Some(deadline),
                None => {
                    return Err(AgentError::new(
                        crate::envelope::Code::Usage,
                        "turn-time",
                        format!(
                            "--turn-time ({}s) is too large to represent as a turn deadline",
                            budget.as_secs()
                        ),
                    ))
                }
            },
            None => None,
        };
        Ok(Self {
            agent,
            started,
            deadline,
            runtime: Some(runtime),
            retry_attempts: std::cell::Cell::new(0),
        })
    }

    pub(super) fn time_exhausted(
        &self,
        store: &SessionStore,
        reserved: &ReservedRequest,
        rounds: &[RoundRecord],
    ) -> AgentError {
        self.dead(
            store,
            reserved,
            "turn-time-exhausted",
            &format!(
                "turn time budget ({}s) exceeded; raise --turn-time or pass 0 to disable",
                self.turn_time_budget
                    .expect("deadline requires a budget")
                    .as_secs()
            ),
            rounds,
        )
    }

    pub(super) fn retry_delay(&self, delay: std::time::Duration) -> Result<(), RoundFailure> {
        self.runtime
            .as_ref()
            .expect("turn runtime")
            .block_on(async {
                match self.deadline {
                    Some(deadline) => tokio::time::timeout_at(deadline, tokio::time::sleep(delay))
                        .await
                        .map_err(|_| RoundFailure::TurnTime),
                    None => {
                        tokio::time::sleep(delay).await;
                        Ok(())
                    }
                }
            })
    }

    pub(super) fn round_once(
        &self,
        requests: &[serdes_ai::core::ModelRequest],
        tools: &[crate::tools::ToolSpec],
        reattempt: bool,
    ) -> Result<LlmResult, RoundFailure> {
        let result = self
            .runtime
            .as_ref()
            .expect("the turn runtime outlives every provider round")
            .block_on(async {
                if self
                    .deadline
                    .is_some_and(|deadline| Instant::now() >= deadline)
                {
                    return Err(RoundFailure::TurnTime);
                }
                if reattempt {
                    self.retry_attempts.set(self.retry_attempts.get() + 1);
                }
                let request = self.backend.request(requests, tools);
                match self.deadline {
                    Some(deadline) => tokio::time::timeout_at(deadline, request)
                        .await
                        .map_err(|_| RoundFailure::TurnTime),
                    None => Ok(request.await),
                }
            })?
            .map_err(|error| {
                if let Some(events) = self.backend.stream_interruption() {
                    return RoundFailure::StreamInterrupted(events);
                }
                match error {
                    crate::adapter::BackendError::Transport(failure) => {
                        RoundFailure::ModelTransport(failure)
                    }
                    crate::adapter::BackendError::Message(message) => RoundFailure::Model(message),
                }
            })?;
        if let (Some(output), Some(snapshot)) =
            (&self.cache_output, self.backend.cache_observation())
        {
            output
                .publish(&snapshot)
                .map_err(RoundFailure::CacheOutput)?;
        }
        // Bound and validate the reply before it can join any transcript or usage total.
        provider_validation::validate(&result, &self.secrets)?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests;
