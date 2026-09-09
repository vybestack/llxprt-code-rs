//! One execution clock and executor for a live turn, shared by every provider call.
//! Setup (including credential lookup and session reservation) precedes this boundary.

use super::*;
use tokio::time::Instant;

/// A replay never constructs this state. Dropping a timed-out request cancels its
/// transport before the caller persists failure. Teardown also cancels any reader
/// tasks spawned by the transport on this executor; no request thread is detached.
pub(super) struct Turn<'a> {
    agent: &'a CodingAgent,
    pub(super) started: Instant,
    pub(super) deadline: Option<Instant>,
    runtime: tokio::runtime::Runtime,
}

impl std::ops::Deref for Turn<'_> {
    type Target = CodingAgent;

    fn deref(&self) -> &Self::Target {
        self.agent
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
            runtime,
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

    pub(super) fn round(
        &self,
        requests: &[serdes_ai::core::ModelRequest],
        tools: &[crate::tools::ToolSpec],
    ) -> Result<LlmResult, RoundFailure> {
        let result = self
            .runtime
            .block_on(async {
                let request = self.backend.request(requests, tools);
                match self.deadline {
                    Some(deadline) => {
                        if Instant::now() >= deadline {
                            return Err(RoundFailure::TurnTime);
                        }
                        tokio::time::timeout_at(deadline, request)
                            .await
                            .map_err(|_| RoundFailure::TurnTime)
                    }
                    None => Ok(request.await),
                }
            })?
            .map_err(|message| match TransportFailure::from_message(&message) {
                Some(failure) => RoundFailure::ModelTransport(failure),
                None => RoundFailure::Model(message),
            })?;
        // Bound and validate the reply before it can join any transcript or usage total.
        validate_provider_result(&result, &self.secrets).map_err(RoundFailure::Model)?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests;
