//! One execution clock and executor for a live turn, shared by every provider call.
//! Setup (including credential lookup and session reservation) precedes this boundary.

use super::*;
use tokio::time::Instant;
mod retry;

/// A replay never constructs this state. Dropping a timed-out request cancels its
/// transport before the caller persists failure. Teardown also cancels any reader
/// tasks spawned by the transport on this executor; no request thread is detached.
pub(super) struct Turn<'a> {
    agent: &'a CodingAgent,
    store: &'a SessionStore,
    reserved: &'a ReservedRequest,
    pub(super) started: Instant,
    pub(super) deadline: Option<Instant>,
    runtime: tokio::runtime::Runtime,
    request_attempts: std::cell::Cell<crate::envelope::RequestAttempts>,
}

impl std::ops::Deref for Turn<'_> {
    type Target = CodingAgent;

    fn deref(&self) -> &Self::Target {
        self.agent
    }
}

impl<'a> Turn<'a> {
    pub(super) fn new(
        agent: &'a CodingAgent,
        store: &'a SessionStore,
        reserved: &'a ReservedRequest,
    ) -> Result<Self, AgentError> {
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
        Ok(Self {
            agent,
            store,
            reserved,
            request_attempts: Default::default(),
            started,
            deadline: agent.turn_time_budget.map(|budget| {
                started
                    .checked_add(budget)
                    .expect("turn deadline must be representable")
            }),
            runtime,
        })
    }

    pub(super) fn run_counted(
        &self,
        store: &SessionStore,
        reserved: &mut ReservedRequest,
    ) -> Result<CompletedRun, AgentError> {
        let mut result = self.run_reserved(store, reserved);
        match &mut result {
            Ok(run) => run.request_attempts = self.request_attempts.get(),
            Err(error) => error.request_attempts = self.request_attempts.get(),
        }
        result
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
        let result = self.runtime.block_on(async {
            let request = self.request_with_retry(requests, tools);
            match self.deadline {
                Some(deadline) => tokio::time::timeout_at(deadline, request)
                    .await
                    .map_err(|_| RoundFailure::TurnTime)?,
                None => request.await,
            }
        })?;
        // Bound and validate the reply before it can join any transcript or usage total.
        validate_provider_result(&result, &self.secrets).map_err(RoundFailure::Model)?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests;
