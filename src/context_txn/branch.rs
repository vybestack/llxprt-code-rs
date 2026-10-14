//! Receipt-checked branch effects use the existing gate, ingress, fence and writer.
use super::{
    budget::Budget,
    executor::{CommitOutcome, Executor, ExecutorError, FencingClock},
    semantic::SemanticServices,
};
use crate::context_kernel::{
    branch::*,
    events::{EventKind, Sequencer},
    ledger::Source,
    legality::is_legal,
    reducer::{Reducer, TypedState, IDLENESS_WINDOW},
    validation::{Candidate, Verdict},
};

/// Authenticated harness/ingress records. Proposers cannot self-declare tool/read receipts.
pub trait BranchPorts {
    fn principal_authenticated(&self, source: &Source, namespace: &str) -> bool;
    fn spawn_authenticated(&self, spawn: &Spawn) -> bool;
    fn round_authenticated(&self, namespace: &str, round: &Round) -> bool;
}
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BranchCommitError {
    #[error("branch receipt/authority boundary refused")]
    Boundary,
    #[error("branch replay refused: {0:?}")]
    Replay(crate::context_kernel::reducer::ReducerError),
    #[error("branch universal transaction refused: {0:?}")]
    Transaction(ExecutorError),
    #[error("branch semantic validation refused")]
    Gate(crate::context_kernel::validation::GateError),
}
pub struct BranchReceipt {
    event: BranchEvent,
    bound: u64,
    principal: Source,
}
impl Executor {
    pub fn governed_store_version(&self) -> u64 {
        self.typed.store_version
    }
    pub fn branches(&self) -> &BranchState {
        &self.typed.branches
    }
    pub fn branch_constitution(&self) -> std::collections::BTreeMap<String, String> {
        self.typed.constitution()
    }
    /// Every non-return effect is authenticated against harness/ingress records.
    pub fn validate_branch(
        &mut self,
        event: BranchEvent,
        principal: Source,
        ports: &dyn BranchPorts,
        budget: &Budget,
    ) -> Result<BranchReceipt, BranchCommitError> {
        let result = (|| {
            if matches!(event, BranchEvent::Return { .. }) {
                return Err(BranchCommitError::Boundary);
            }
            self.branch_scope(&event, &principal)?;
            authenticate(&event, &principal, ports)?;
            self.branch_receipt(event, principal, budget)
        })();
        if result.is_err() {
            self.fail();
        }
        result
    }
    /// Return is always evidential. Gate pass plus exact derivation ingress mint the receipt.
    pub fn validate_branch_return(
        &mut self,
        namespace: &str,
        candidate: Candidate,
        principal: Source,
        services: SemanticServices<'_>,
        ports: &dyn BranchPorts,
        budget: &Budget,
    ) -> Result<BranchReceipt, BranchCommitError> {
        let result = (|| {
            if principal.origin != crate::context_kernel::ledger::Origin::Ingress
                || principal.authority < crate::context_kernel::ledger::Authority::Agent
                || !ports.principal_authenticated(&principal, namespace)
                || candidate.scope != principal.scope
            {
                return Err(BranchCommitError::Boundary);
            }
            let branch = self
                .branches()
                .branches()
                .get(namespace)
                .ok_or(BranchCommitError::Boundary)?;
            if branch.spawn.scope != candidate.scope || !candidate.rendered.is_empty() {
                return Err(BranchCommitError::Boundary);
            }
            let record = services
                .gate
                .validate(
                    &candidate,
                    services.required,
                    services.always_checked,
                    services.store,
                    services.consumers,
                )
                .map_err(BranchCommitError::Gate)?;
            if record.verdict != Verdict::Pass {
                return Err(BranchCommitError::Boundary);
            }
            services
                .ingress
                .capture_derivation("branch-return", candidate.artifact.as_bytes())
                .map_err(|_| BranchCommitError::Boundary)?;
            let records = services
                .ingress
                .commit(services.sink)
                .map_err(|_| BranchCommitError::Boundary)?;
            let admitted = super::semantic::exact_derivation(&records, &candidate.artifact)
                .map_err(|_| BranchCommitError::Boundary)?;
            let (status, revalidate) = self
                .branches()
                .return_status(namespace, self.ledger().versions())
                .map_err(|_| BranchCommitError::Boundary)?;
            let event = BranchEvent::Return {
                namespace: namespace.into(),
                returned: Returned {
                    handle: admitted.payload.handle.clone(),
                    digest: crate::context_kernel::security::hash(candidate.artifact.as_bytes()),
                    status,
                    revalidate,
                },
            };
            self.branch_receipt(event, principal, budget)
        })();
        if result.is_err() {
            self.fail();
        }
        result
    }
    fn branch_scope(
        &self,
        event: &BranchEvent,
        principal: &Source,
    ) -> Result<(), BranchCommitError> {
        let scope = match event {
            BranchEvent::Open(spawn) => Some(spawn.scope),
            BranchEvent::Round { namespace, .. }
            | BranchEvent::Return { namespace, .. }
            | BranchEvent::Abort { namespace } => self
                .branches()
                .branches()
                .get(namespace)
                .map(|b| b.spawn.scope),
        };
        if scope != Some(principal.scope)
            || !self
                .typed
                .scope_registry
                .state(principal.scope)
                .is_ok_and(|s| s.is_open())
        {
            return Err(BranchCommitError::Boundary);
        }
        Ok(())
    }
    fn branch_candidate(&self, event: &BranchEvent) -> Result<TypedState, BranchCommitError> {
        if let BranchEvent::Open(spawn) = event {
            if spawn.parent_version != self.typed.store_version
                || spawn.notes
                    != self
                        .typed
                        .semantic
                        .artifacts()
                        .iter()
                        .filter(|(_, a)| !a.rendered)
                        .map(|(key, _)| key.clone())
                        .collect()
            {
                return Err(BranchCommitError::Boundary);
            }
        }
        let mut log = self.log.clone();
        let mut writer = Sequencer::continuing(&log, self.epoch.0, 1_000);
        log.append(writer.append(
            EventKind::Branch {
                event: Box::new(event.clone()),
            },
            log.store_version(),
        ))
        .map_err(|_| BranchCommitError::Boundary)?;
        let mut candidate = self.typed.clone();
        Reducer::new(IDLENESS_WINDOW)
            .fold_from(&mut candidate, &log)
            .map_err(BranchCommitError::Replay)?;
        Ok(candidate)
    }
    fn branch_receipt(
        &mut self,
        event: BranchEvent,
        principal: Source,
        budget: &Budget,
    ) -> Result<BranchReceipt, BranchCommitError> {
        if self
            .current
            .as_ref()
            .is_none_or(|t| t.op != event.operation())
            || self.proposed_by.is_none()
        {
            return Err(BranchCommitError::Boundary);
        }
        let candidate = self.branch_candidate(&event)?;
        is_legal(&candidate, &self.contract).map_err(|_| BranchCommitError::Boundary)?;
        let bytes = serde_json::to_vec(&event)
            .map_err(|_| BranchCommitError::Boundary)?
            .len() as u64;
        let bound = self
            .port_bound(0, bytes)
            .ok_or(BranchCommitError::Boundary)?;
        self.validate(bound, budget, 0, 0, 0, bytes)
            .map_err(BranchCommitError::Transaction)?;
        if !super::budget::fits(bound, budget) {
            return Err(BranchCommitError::Boundary);
        }
        self.branch_validation = Some((event.clone(), bound));
        Ok(BranchReceipt {
            event,
            bound,
            principal,
        })
    }
    pub fn commit_branch(
        &mut self,
        parent: u64,
        clock: &FencingClock,
        receipt: BranchReceipt,
        ports: &dyn BranchPorts,
    ) -> Result<CommitOutcome, BranchCommitError> {
        let result = (|| {
            self.branch_scope(&receipt.event, &receipt.principal)?;
            authenticate(&receipt.event, &receipt.principal, ports)?;
            if clock.latest() > self.epoch.0
                || self.branch_validation.as_ref() != Some(&(receipt.event.clone(), receipt.bound))
                || self
                    .current
                    .as_ref()
                    .is_none_or(|t| t.op != receipt.event.operation())
            {
                return Err(BranchCommitError::Boundary);
            }
            self.branch_candidate(&receipt.event)?;
            self.commit_checked(
                parent,
                Some(EventKind::Branch {
                    event: Box::new(receipt.event),
                }),
                receipt.bound,
            )
            .map_err(BranchCommitError::Transaction)
        })();
        self.branch_validation = None;
        if result.is_err() {
            self.fail();
        }
        result
    }
    /// Recorded prefix for cold replay. No mutable log capability leaves the executor.
    pub fn governed_log(&self) -> &crate::context_kernel::events::EventLog {
        &self.log
    }
}
fn authenticate(
    event: &BranchEvent,
    principal: &Source,
    ports: &dyn BranchPorts,
) -> Result<(), BranchCommitError> {
    let namespace = match event {
        BranchEvent::Open(s) => &s.namespace,
        BranchEvent::Round { namespace, .. }
        | BranchEvent::Return { namespace, .. }
        | BranchEvent::Abort { namespace } => namespace,
    };
    let scope_valid = match event {
        BranchEvent::Open(s) => s.scope == principal.scope,
        _ => true,
    };
    let valid = scope_valid
        && principal.origin == crate::context_kernel::ledger::Origin::Ingress
        && principal.authority >= crate::context_kernel::ledger::Authority::Agent
        && ports.principal_authenticated(principal, namespace)
        && match event {
            BranchEvent::Open(s) => ports.spawn_authenticated(s),
            BranchEvent::Round { namespace, round } => ports.round_authenticated(namespace, round),
            _ => true,
        };
    if valid {
        Ok(())
    } else {
        Err(BranchCommitError::Boundary)
    }
}
