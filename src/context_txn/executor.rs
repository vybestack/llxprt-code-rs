//! Single-writer, fenced transaction executor skeleton (design §8.3-8.4).
//!
//! State machine: `proposed -> snapshotted -> generated -> validated ->
//! committed | aborted`. Artifacts are durable at `generated`, verdicts at
//! `validated`; commit is a compare-and-commit against the named parent
//! version. Rebase-safe rows re-apply on the actual parent, every other row
//! aborts and must be re-proposed. The universal commit preconditions are
//! enforced *centrally* here, for every row.
//!
//! Unit B: the executor owns the bound (via [`AccountingPort`], #104), owns
//! the protection floor M (computed from governed state), and owns the
//! legality gate (#105) - it runs `is_legal` at the commit gate against the
//! render contract recorded on the transaction, so a projection that exceeds
//! a region budget or carries an unpaired tool declaration fails commit with
//! a typed legality error.

use crate::context_kernel::events::EventLog;
use crate::context_kernel::legality::{is_legal, RenderContract, Violation};
use crate::context_kernel::reducer::{Reducer, TypedState, IDLENESS_WINDOW};

use super::budget::{self, AccountingPort, Budget, Margins};
use super::operation::{self, Proposer};

/// Fenced epoch; monotonic, single writer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Epoch(pub u64);

/// Monotonic fencing clock backing lease acquisition (design S8.3).
pub struct FencingClock {
    latest: std::cell::Cell<u64>,
}

impl FencingClock {
    pub fn new() -> Self {
        FencingClock {
            latest: std::cell::Cell::new(0),
        }
    }

    /// Acquire the next lease epoch; strictly monotonic.
    pub fn acquire(&self) -> Epoch {
        let e = self.latest.get() + 1;
        self.latest.set(e);
        Epoch(e)
    }

    /// Highest lease ever issued.
    pub fn latest(&self) -> u64 {
        self.latest.get()
    }
}

impl Default for FencingClock {
    fn default() -> Self {
        Self::new()
    }
}

/// Transaction lifecycle states.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Txn {
    /// Executor-assigned id.
    pub id: u64,
    /// Parent version this txn commits against.
    pub parent_version: u64,
    /// Operation name from the closed registry.
    pub op: &'static str,
}

/// Verdicts and failures the executor can produce.
#[derive(Clone, PartialEq, Eq, Debug)]
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
    /// The caller-supplied bound disagrees with the port's computed bound
    /// (#104): validation is refused, the caller cannot invent a number.
    BoundDisagrees {
        /// Bound the caller claimed.
        claimed: u64,
        /// Bound the port computed.
        computed: u64,
    },
    /// The legality gate rejected the committed projection (#105); `which`
    /// names the violated predicate kind, `predicate` its text.
    Illegal {
        /// Violated predicate kind.
        which: &'static str,
        /// Text of the violated predicate.
        predicate: String,
    },
    /// Region-wide admission accounting (#121-c): the projected region
    /// occupancy exceeds the region budget net of reserve and headroom.
    RegionOverBudget {
        /// Occupancy already admitted in the region.
        admitted: u64,
        /// Occupancy this admission projects.
        projected: u64,
        /// Region budget net of reserve and headroom.
        ceiling: u64,
    },
    /// A newer lease exists; this executor's epoch is fenced out.
    Fenced {
        /// Epoch currently held.
        held: u64,
        /// Our fenced-out epoch.
        mine: u64,
    },
    /// Acting principal exceeds the row's registered authority.
    AuthorityDenied { op: &'static str, by: Proposer },
    /// Non-rebase-safe row asked to re-apply.
    NotRebaseSafe,
}

/// What `commit` did to the governed state: applied a real effect, or
/// re-applied a rebase-safe row on a moved parent and turned out to be a
/// no-op the caller must not report as progress.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitOutcome {
    /// The effect landed against the named parent.
    Applied,
    /// The row is rebase-safe but the parent moved; the caller re-applies it
    /// out of band, so this commit produced no effect of its own.
    RebaseNoOp,
}

/// Fenced executor. The `epoch` fences stale writers; only the highest epoch
/// may commit. One executor is long-lived across admissions (#121-b): the
/// session owns it, the epoch comes from the fencing clock, and it is never
/// rebuilt per admission - a fresh `Epoch(1)` per call could never fence a
/// stale writer out.
///
/// The render contract is bound to the executor (one per session) and its
/// version is carried on the transaction (#105); the governed state it gates
/// commits against is folded here, from the events the session's admissions
/// appended, so `M` and the bound come from governed state and not from the
/// caller (#104).
#[derive(Clone)]
pub struct Executor {
    epoch: Epoch,
    next_id: u64,
    state: TxnState,
    current: Option<Txn>,
    aborted: bool,
    port: Option<std::rc::Rc<dyn AccountingPort>>,
    margins: Margins,
    contract: RenderContract,
    log: EventLog,
    typed: TypedState,
    /// Contract version committed with the last applied effect; the durable
    /// record carries it and the send path compares against it (#105-3).
    contract_version: Option<u64>,
    region_admitted: u64,
    region_ceiling: Option<u64>,
    landed_phase: u8,
}

impl Executor {
    /// New long-lived executor at `epoch`, gating commits against `contract`.
    pub fn new(epoch: Epoch, contract: RenderContract) -> Self {
        let log = EventLog::new(crate::context_kernel::migration::V2);
        let typed = Reducer::new(IDLENESS_WINDOW)
            .fold(&log)
            .expect("an empty log folds into genesis");
        Executor {
            epoch,
            next_id: 1,
            state: TxnState::Proposed,
            current: None,
            aborted: false,
            port: None,
            margins: Margins::V1,
            contract,
            log,
            typed,
            contract_version: None,
            region_admitted: 0,
            region_ceiling: None,
            landed_phase: 4,
        }
    }

    /// Constructor for callers that have no contract yet: a generous contract
    /// at version 1.
    pub fn new_generous(epoch: Epoch) -> Self {
        Self::new(epoch, RenderContract::generous(1))
    }

    /// Binds the accounting port: from here on the caller cannot invent a
    /// bound, `validate` computes it from the port (#104).
    pub fn bind_port(&mut self, port: std::rc::Rc<dyn AccountingPort>) {
        self.port = Some(port);
    }

    /// The versioned margin table the executor charges (#104-3). Usage
    /// reconciliation against a measured number is blocked by #80; what is
    /// landed is the versioning plus the drift fixture.
    pub fn margins(&self) -> Margins {
        self.margins
    }

    /// Recalibrates the margin table on detected drift (#104-3): the version
    /// must be strictly newer, so replay never silently adopts a newer table.
    pub fn recalibrate_margins(
        &mut self,
        version: u64,
        per_tool_declaration: u64,
    ) -> Result<(), &'static str> {
        self.margins = self.margins.recalibrate(version, per_tool_declaration)?;
        Ok(())
    }

    /// The render contract the executor gates commits against (#105).
    pub fn contract(&self) -> &RenderContract {
        &self.contract
    }

    /// The contract version committed with the last applied effect; the
    /// durable record and the send path compare against it (#105-3).
    pub fn committed_contract_version(&self) -> Option<u64> {
        self.contract_version
    }

    /// Event count in the executor's commit log. The durable log is only
    /// appended by the test fixture today; the commit-log seam it carries is
    /// owned by a later unit, so this is the natural read for callers that
    /// must observe it.
    pub fn commit_log_len(&self) -> usize {
        self.log.len()
    }

    /// Arms region-wide admission accounting (#121-c): `ceiling` is the region
    /// budget net of reserve and headroom; every applied admission's projected
    /// occupancy is summed against it.
    pub fn arm_region_accounting(&mut self, ceiling: u64) {
        self.region_ceiling = Some(ceiling);
        self.region_admitted = 0;
    }

    /// Units already admitted into the region (#121-c).
    pub fn region_admitted(&self) -> u64 {
        self.region_admitted
    }

    /// Test-only: folds one placed item into the governed state so the
    /// legality gate has a projection to reject.
    #[cfg(test)]
    pub fn ingest_test_item(&mut self) {
        use crate::context_kernel::events::{
            AppendSource, EventKind, OperationClass, Sequencer, FIRST_SEQUENCE,
        };
        let mut sequencer = Sequencer::new(FIRST_SEQUENCE, self.log.store_version(), 1_000);
        let scope_open = sequencer.append(
            EventKind::OperationCommit {
                class: OperationClass::ScopeOpen,
                subject: 1,
                argument: 0,
            },
            self.log.store_version(),
        );
        self.log.append(scope_open).expect("scope open appends");
        let append = sequencer.append(
            EventKind::Append {
                source: AppendSource::User,
                sanitized: b"head".to_vec(),
                scope: 1,
                claims: Vec::new(),
            },
            self.log.store_version(),
        );
        self.log.append(append).expect("append appends");
        let place = sequencer.append(
            EventKind::OperationCommit {
                class: OperationClass::Place,
                subject: 0,
                argument: crate::context_kernel::ir::Region::Head.rank(),
            },
            self.log.store_version(),
        );
        self.log.append(place).expect("place appends");
        let reducer = Reducer::new(IDLENESS_WINDOW);
        reducer
            .fold_from(&mut self.typed, &self.log)
            .expect("the test log folds");
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

    /// Generate effect artifacts (durable). Rows through Phase 4 are landed;
    /// rows owned by a later phase stop with a typed verdict and kill the
    /// transaction, so a capability failure can never be committed.
    pub fn generate(&mut self) -> Result<Txn, ExecutorError> {
        self.step(TxnState::Generated)?;
        let txn = self.current.clone().expect("txn present after propose");
        let row = operation::find(txn.op).expect("registered");
        if row.owner_phase > self.landed_phase {
            self.fail();
            return Err(ExecutorError::CapabilityNotLanded { op: txn.op });
        }
        Ok(txn)
    }

    /// Test-only: advances the landed-phase gate so a row owned by a later
    /// phase can be exercised end to end in a fixture. Production callers
    /// never move this; the default lands rows through Phase 4.
    #[cfg(test)]
    pub fn land_through(&mut self, phase: u8) {
        self.landed_phase = phase;
    }

    /// Kills the live transaction after a failed precondition or capability
    /// check: the state machine jumps to `Aborted` and every further step is
    /// refused until a new transaction is proposed.
    fn fail(&mut self) {
        self.aborted = true;
        self.state = TxnState::Aborted;
    }

    /// Record verdicts and enforce the universal preconditions centrally.
    ///
    /// #104: the bound comes from the bound [`AccountingPort`], never from
    /// the caller. `claimed_bound` is what the caller believes the commit
    /// costs; it must equal the port's computed bound or validation fails
    /// with [`ExecutorError::BoundDisagrees`]. `m` is the protection floor
    /// and must still satisfy `M + R + H <= B`, and the reclamation bar is
    /// checked against the row's registered (nonzero) bar.
    ///
    /// Any failure leaves the transaction dead: a failed validate can never
    /// be repaired in place, so the only next legal step is `abort`.
    pub fn validate(
        &mut self,
        claimed_bound: u64,
        budget: &Budget,
        m: u64,
        phi_pre: u64,
        phi_post: u64,
    ) -> Result<Txn, ExecutorError> {
        self.step(TxnState::Validated)?;
        let txn = self.current.clone().expect("txn present after propose");
        let row = operation::find(txn.op).expect("registered");
        let Some(port) = self.port.clone() else {
            self.fail();
            return Err(ExecutorError::PreconditionFailed {
                which: "bound-port-missing",
            });
        };
        let computed = port.bound(self.typed.store_version, claimed_bound);
        if computed != claimed_bound {
            self.fail();
            return Err(ExecutorError::BoundDisagrees {
                claimed: claimed_bound,
                computed,
            });
        }
        if !budget::fits(computed, budget) {
            self.fail();
            return Err(ExecutorError::PreconditionFailed { which: "fit-bound" });
        }
        if !budget::feasible(m, budget) {
            self.fail();
            return Err(ExecutorError::PreconditionFailed {
                which: "protection-floor",
            });
        }
        if row.reclamation && !budget::net_reclaim_ok(phi_pre, phi_post, row.bar) {
            self.fail();
            return Err(ExecutorError::PreconditionFailed {
                which: "reclamation-bar",
            });
        }
        Ok(txn)
    }

    /// Compare-and-commit with the legality gate (#105): `is_legal` runs over
    /// the folded governed state against the executor's render contract
    /// before the parent check, so an over-budget projection or an unpaired
    /// tool declaration fails commit with a typed legality error. The
    /// contract version the gate accepted is recorded on the executor so the
    /// durable record and the send path can compare against it.
    pub fn commit_outcome(&mut self, actual_parent: u64) -> Result<CommitOutcome, ExecutorError> {
        if !self.state.can_transition(TxnState::Committed) {
            return Err(ExecutorError::IllegalTransition {
                from: self.state,
                to: TxnState::Committed,
            });
        }
        let txn = self.current.clone().expect("txn present after propose");
        let row = operation::find(txn.op).expect("registered");
        if let Err(violation) = is_legal(&self.typed, &self.contract) {
            self.fail();
            return Err(illegality(violation));
        }
        if actual_parent != txn.parent_version {
            if row.rebase_safe {
                // The transaction ends committed-for-replay purposes, but no
                // effect was produced here: the caller re-applies on the moved
                // parent and must not count this commit as applied progress.
                self.state = TxnState::Committed;
                return Ok(CommitOutcome::RebaseNoOp);
            }
            self.fail();
            return Err(ExecutorError::StaleParent {
                expected: txn.parent_version,
                actual: actual_parent,
            });
        }
        if let Some(ceiling) = self.region_ceiling {
            let admitted = self.region_admitted;
            if admitted > ceiling {
                self.fail();
                return Err(ExecutorError::RegionOverBudget {
                    admitted,
                    projected: admitted,
                    ceiling,
                });
            }
        }
        self.state = TxnState::Committed;
        self.contract_version = Some(self.contract.version);
        Ok(CommitOutcome::Applied)
    }

    /// Compare-and-commit, reporting the outcome. A [`CommitOutcome::RebaseNoOp`]
    /// is returned to the caller, never collapsed into a committed state
    /// (#121-a): the caller must not report a no-op as applied progress.
    pub fn commit(&mut self, actual_parent: u64) -> Result<CommitOutcome, ExecutorError> {
        self.commit_outcome(actual_parent)
    }

    /// Fenced compare-and-commit: a newer lease epoch fences us out
    /// before the parent check runs (design S8.3). The outcome is returned,
    /// not collapsed (#121-a).
    pub fn commit_fenced(
        &mut self,
        actual_parent: u64,
        clock: &FencingClock,
    ) -> Result<CommitOutcome, ExecutorError> {
        if clock.latest() > self.epoch.0 {
            self.fail();
            return Err(ExecutorError::Fenced {
                held: clock.latest(),
                mine: self.epoch.0,
            });
        }
        self.commit(actual_parent)
    }

    /// Authority non-increase: the acting principal must be the row's
    /// proposer or the named higher authority (tab:ops authority column).
    pub fn propose_as(
        &mut self,
        op: &'static str,
        parent_version: u64,
        by: Proposer,
    ) -> Result<Txn, ExecutorError> {
        let row = match operation::find(op) {
            Some(row) => row,
            None => return Err(ExecutorError::CapabilityNotLanded { op }),
        };
        let sanctioned = by == row.proposer || row.authority == Some(by);
        if !sanctioned {
            return Err(ExecutorError::AuthorityDenied { op, by });
        }
        self.propose(op, parent_version)
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

/// Maps a legality violation onto the typed executor error (#105): the kind
/// names the violated predicate so a rejected commit is explainable.
fn illegality(violation: Violation) -> ExecutorError {
    let which = match violation {
        Violation::Pairing { .. } => "pairing",
        Violation::Ordering { .. } => "ordering",
        Violation::PlaceholderIllegal { .. } => "placeholder",
        Violation::RegionOverBudget { .. } => "region-over-budget",
        Violation::Floor { .. } => "floor",
        Violation::Pin { .. } => "pin",
        Violation::QuotingConvention { .. } => "quoting-convention",
    };
    ExecutorError::Illegal {
        which,
        predicate: violation.predicate().to_string(),
    }
}
