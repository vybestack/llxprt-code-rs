//! Receipt-checked ledger commits reuse the executor's universal gate.
use super::executor::{CommitOutcome, Executor, ExecutorError, FencingClock};
use crate::context_kernel::{
    events::{EventKind, Sequencer},
    ledger::{Command, LedgerPorts},
    reducer::{Reducer, IDLENESS_WINDOW},
};

/// Failures preserve the distinction between domain/authentication, replay and
/// universal transaction gates. None is reported as an applied commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LedgerCommitError {
    Domain(crate::context_kernel::ledger::LedgerError),
    Boundary,
    Replay(crate::context_kernel::reducer::ReducerError),
    Transaction(ExecutorError),
}
/// Typed command binding: generic validation cannot admit different bytes.
#[derive(Clone)]
pub(super) struct CommandValidation(pub Command);
impl Executor {
    /// Compute actual ledger effect size and rendered reclaim internally. The
    /// caller supplies its registered budget, never a utility/floor/reclaim verdict.
    pub fn validate_ledger(
        &mut self,
        command: &Command,
        budget: &super::budget::Budget,
    ) -> Result<u64, LedgerCommitError> {
        let candidate = match self.ledger_candidate(command) {
            Ok(candidate) => candidate,
            Err(error) => {
                self.fail();
                return Err(error);
            }
        };
        let bytes = serde_json::to_vec(command)
            .expect("integer command encodes")
            .len() as u64;
        let Some(bound) = self.port_bound(0, bytes) else {
            self.fail();
            return Err(LedgerCommitError::Boundary);
        };
        let floor: u64 = candidate
            .ledger
            .entries()
            .filter(|entry| {
                !entry.retired
                    && entry.class == crate::context_kernel::ledger::FactClass::Obligation
            })
            .map(|entry| entry.fold.text.len() as u64)
            .sum();
        let before = rendered_bytes(&self.typed.ledger);
        let after = rendered_bytes(&candidate.ledger);
        self.validate(bound, budget, floor, before, after, bytes)
            .map_err(LedgerCommitError::Transaction)?;
        if !super::budget::fits(bound, budget) {
            self.fail();
            return Err(LedgerCommitError::Transaction(
                ExecutorError::PreconditionFailed {
                    which: "ledger-fit-bound",
                },
            ));
        }
        self.ledger_validation = Some((CommandValidation(command.clone()), bound));
        Ok(bound)
    }
    fn ledger_candidate(
        &self,
        command: &Command,
    ) -> Result<crate::context_kernel::reducer::TypedState, LedgerCommitError> {
        let mut log = self.log.clone();
        let mut writer = Sequencer::continuing(&log, self.epoch.0, 1_000);
        let effect = EventKind::Ledger {
            command: Box::new(command.clone()),
        };
        log.append(writer.append(effect, log.store_version()))
            .expect("continuing checked log");
        let mut candidate = self.typed.clone();
        Reducer::new(IDLENESS_WINDOW)
            .fold_from(&mut candidate, &log)
            .map_err(LedgerCommitError::Replay)?;
        Ok(candidate)
    }
    /// The only ledger commit entry point. Caller cannot claim a proposer:
    /// the operation is bound by `propose_as`, and the command must match it.
    /// Atomic write-result/invalidation is represented by one sequenced command.
    pub fn commit_ledger(
        &mut self,
        actual_parent: u64,
        clock: &FencingClock,
        command: Command,
        ports: &dyn LedgerPorts,
        payload_bound: u64,
    ) -> Result<CommitOutcome, LedgerCommitError> {
        let bound_matches = self
            .ledger_validation
            .as_ref()
            .is_some_and(|(validated, bound)| validated.0 == command && *bound == payload_bound);
        if !bound_matches {
            self.fail();
            return Err(LedgerCommitError::Boundary);
        }
        let result = self.check_ledger_command(clock, &command, ports);
        if let Err(error) = result {
            self.fail();
            return Err(error);
        }
        let candidate = match self.ledger_candidate(&command) {
            Ok(candidate) => candidate,
            Err(error) => {
                self.fail();
                return Err(error);
            }
        };
        let effect = EventKind::Ledger {
            command: Box::new(command),
        };
        if let Err(violation) =
            crate::context_kernel::legality::is_legal(&candidate, &self.contract)
        {
            self.fail();
            return Err(LedgerCommitError::Transaction(super::executor::illegality(
                violation,
            )));
        }
        self.commit_checked(actual_parent, Some(effect), payload_bound)
            .map_err(LedgerCommitError::Transaction)
    }
    fn check_ledger_command(
        &self,
        clock: &FencingClock,
        command: &Command,
        ports: &dyn LedgerPorts,
    ) -> Result<(), LedgerCommitError> {
        if clock.latest() > self.epoch.0 {
            return Err(LedgerCommitError::Transaction(ExecutorError::Fenced {
                held: clock.latest(),
                mine: self.epoch.0,
            }));
        }
        let txn = self.current.as_ref().ok_or(LedgerCommitError::Boundary)?;
        if txn.op != command.operation() || self.proposed_by.is_none() {
            return Err(LedgerCommitError::Boundary);
        }
        command
            .authenticate(ports)
            .map_err(LedgerCommitError::Domain)
    }
    /// Read port only; no controller mutable ledger capability exists.
    pub fn ledger(&self) -> &crate::context_kernel::ledger::Ledger {
        &self.typed.ledger
    }
}

fn rendered_bytes(ledger: &crate::context_kernel::ledger::Ledger) -> u64 {
    ledger
        .entries()
        .filter(|entry| !entry.retired && entry.rendered)
        .map(|entry| entry.fold.text.len() as u64)
        .sum()
}
