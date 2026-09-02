//! Single-writer, fenced transaction executor skeleton (design §8.3-8.4).
//!
//! State machine: `proposed -> snapshotted -> generated -> validated ->
//! committed | aborted`. Artifacts are durable at `generated`, verdicts at
//! `validated`; commit is a compare-and-commit against the named parent
//! version. Rebase-safe rows re-apply on the actual parent, every other row
//! aborts and must be re-proposed. The universal commit preconditions are
//! enforced *centrally* here, for every row.

use super::budget::{self, Budget};
use super::operation;

/// Fenced epoch; monotonic, single writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Epoch(pub u64);

/// Transaction lifecycle states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxnState {
    /// Proposed, not yet snapshotted.
    Proposed,
    /// Parent snapshot taken.
    Snapshotted,
    /// Effect artifacts generated (durable).
    Generated,
    /// Verdicts recorded (durable).
    Validated,
    /// Committed against the named parent.
    Committed,
    /// Aborted (or never landed).
    Aborted,
}

impl TxnState {
    fn index(self) -> u8 {
        match self {
            TxnState::Proposed => 0,
            TxnState::Snapshotted => 1,
            TxnState::Generated => 2,
            TxnState::Validated => 3,
            TxnState::Committed => 4,
            TxnState::Aborted => 5,
        }
    }

    /// Only forward, adjacent steps are legal; abort is reachable from any
    /// non-terminal state.
    pub fn can_transition(self, to: TxnState) -> bool {
        let terminal = self == TxnState::Committed || self == TxnState::Aborted;
        if to == TxnState::Aborted {
            return !terminal;
        }
        !terminal && to.index() == self.index() + 1
    }
}

/// A transaction under execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Txn {
    /// Executor-assigned id.
    pub id: u64,
    /// Parent version this txn commits against.
    pub parent_version: u64,
    /// Operation name from the closed registry.
    pub op: &'static str,
}

/// Verdicts and failures the executor can produce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutorError {
    /// The requested step is not part of the state machine.
    IllegalTransition {
        /// State the txn is in.
        from: TxnState,
        /// State that was requested.
        to: TxnState,
    },
    /// Parent version moved under us and the row is not rebase-safe.
    StaleParent {
        /// Version we snapshotted.
        expected: u64,
        /// Version observed at commit.
        actual: u64,
    },
    /// Row is registered but owned by a later phase.
    CapabilityNotLanded { op: &'static str },
    /// A universal precondition failed; `which` names it.
    PreconditionFailed { which: &'static str },
    /// Non-rebase-safe row asked to re-apply.
    NotRebaseSafe,
}

/// Fenced executor. The `epoch` fences stale writers; only the highest epoch
/// may commit.
#[derive(Debug, Clone)]
pub struct Executor {
    epoch: Epoch,
    next_id: u64,
    state: TxnState,
    current: Option<Txn>,
    aborted: bool,
}

impl Executor {
    /// New executor at `epoch`.
    pub fn new(epoch: Epoch) -> Self {
        Executor {
            epoch,
            next_id: 1,
            state: TxnState::Proposed,
            current: None,
            aborted: false,
        }
    }

    /// Our fence.
    pub fn epoch(&self) -> Epoch {
        self.epoch
    }

    fn step(&mut self, to: TxnState) -> Result<(), ExecutorError> {
        if self.aborted {
            return Err(ExecutorError::IllegalTransition {
                from: self.state,
                to,
            });
        }
        if !self.state.can_transition(to) {
            return Err(ExecutorError::IllegalTransition {
                from: self.state,
                to,
            });
        }
        self.state = to;
        Ok(())
    }

    /// Begin a transaction for registered row `op` against `parent_version`.
    pub fn propose(&mut self, op: &'static str, parent_version: u64) -> Result<Txn, ExecutorError> {
        if operation::find(op).is_none() {
            return Err(ExecutorError::CapabilityNotLanded { op });
        }
        if self.state != TxnState::Proposed && self.state != TxnState::Committed {
            return Err(ExecutorError::IllegalTransition {
                from: self.state,
                to: TxnState::Proposed,
            });
        }
        let txn = Txn {
            id: self.next_id,
            parent_version,
            op,
        };
        self.next_id += 1;
        self.current = Some(txn.clone());
        self.state = TxnState::Proposed;
        self.aborted = false;
        Ok(txn)
    }

    /// Take the parent snapshot.
    pub fn snapshot(&mut self) -> Result<Txn, ExecutorError> {
        self.step(TxnState::Snapshotted)?;
        Ok(self.current.clone().expect("txn present after propose"))
    }

    /// Generate effect artifacts (durable). Rows owned by a later phase stop
    /// here with a typed `capability_not_landed` verdict.
    pub fn generate(&mut self) -> Result<Txn, ExecutorError> {
        self.step(TxnState::Generated)?;
        let txn = self.current.clone().expect("txn present after propose");
        let row = operation::find(txn.op).expect("registered");
        if row.owner_phase != 3 {
            return Err(ExecutorError::CapabilityNotLanded { op: txn.op });
        }
        Ok(txn)
    }

    /// Record verdicts and enforce the universal preconditions centrally.
    pub fn validate(
        &mut self,
        bound: u64,
        budget: &Budget,
        m: u64,
        phi_pre: u64,
        phi_post: u64,
    ) -> Result<Txn, ExecutorError> {
        self.step(TxnState::Validated)?;
        if !budget::fits(bound, budget) {
            return Err(ExecutorError::PreconditionFailed { which: "fit-bound" });
        }
        if !budget::feasible(m, budget) {
            return Err(ExecutorError::PreconditionFailed {
                which: "protection-floor",
            });
        }
        let txn = self.current.clone().expect("txn present after propose");
        let row = operation::find(txn.op).expect("registered");
        if row.reclamation && !budget::net_reclaim_ok(phi_pre, phi_post, row.bar) {
            return Err(ExecutorError::PreconditionFailed {
                which: "reclamation-bar",
            });
        }
        Ok(txn)
    }

    /// Compare-and-commit: rebase-safe rows re-apply on the actual parent,
    /// every other row aborts on mismatch.
    pub fn commit(&mut self, actual_parent: u64) -> Result<TxnState, ExecutorError> {
        if !self.state.can_transition(TxnState::Committed) {
            return Err(ExecutorError::IllegalTransition {
                from: self.state,
                to: TxnState::Committed,
            });
        }
        let txn = self.current.clone().expect("txn present after propose");
        let row = operation::find(txn.op).expect("registered");
        if actual_parent != txn.parent_version {
            if row.rebase_safe {
                return Ok(TxnState::Committed);
            }
            self.aborted = true;
            self.state = TxnState::Aborted;
            return Err(ExecutorError::StaleParent {
                expected: txn.parent_version,
                actual: actual_parent,
            });
        }
        self.state = TxnState::Committed;
        Ok(TxnState::Committed)
    }

    /// Abort the current transaction.
    pub fn abort(&mut self) -> Result<TxnState, ExecutorError> {
        if !self.state.can_transition(TxnState::Aborted) {
            return Err(ExecutorError::IllegalTransition {
                from: self.state,
                to: TxnState::Aborted,
            });
        }
        self.aborted = true;
        self.state = TxnState::Aborted;
        Ok(TxnState::Aborted)
    }

    /// Current state (for property tests).
    pub fn state(&self) -> TxnState {
        self.state
    }
}
