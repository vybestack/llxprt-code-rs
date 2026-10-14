//! Security operations cannot be committed through generic row arguments.
use super::{
    budget::Budget,
    executor::{CommitOutcome, Executor, ExecutorError, FencingClock},
};
use crate::context_kernel::{
    events::{EventKind, Sequencer},
    legality::is_legal,
    reducer::{Reducer, IDLENESS_WINDOW},
    security::*,
};

/// Authentication, frozen envelope and physical erasure belong to their existing owners.
pub trait SecurityPorts {
    fn principal_authenticated(&self, principal: &crate::context_kernel::ledger::Source) -> bool;
    fn escalation_authorized(&self, request: &Escalation) -> bool;
    /// Must verify vault tombstone AND a durable post-erasure checkpoint, not model claims.
    fn erasure_completed(&self, erasure: &Erasure) -> bool;
}
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SecurityCommitError {
    #[error("security receipt/authority boundary refused")]
    Boundary,
    #[error("security replay refused: {0:?}")]
    Replay(crate::context_kernel::reducer::ReducerError),
    #[error("security transaction refused: {0:?}")]
    Transaction(ExecutorError),
}
pub struct SecurityReceipt {
    event: SecurityEvent,
    bound: u64,
}
impl Executor {
    pub fn security(&self) -> &SecurityState {
        &self.typed.security
    }
    pub fn validate_security(
        &mut self,
        event: SecurityEvent,
        ports: &dyn SecurityPorts,
        budget: &Budget,
    ) -> Result<SecurityReceipt, SecurityCommitError> {
        let result = (|| {
            self.authenticate_security(&event, ports)?;
            self.security_candidate(&event)?;
            let bytes = serde_json::to_vec(&event)
                .map_err(|_| SecurityCommitError::Boundary)?
                .len() as u64;
            let bound = self
                .port_bound(0, bytes)
                .ok_or(SecurityCommitError::Boundary)?;
            self.validate(bound, budget, 0, 0, 0, bytes)
                .map_err(SecurityCommitError::Transaction)?;
            if !super::budget::fits(bound, budget) {
                return Err(SecurityCommitError::Boundary);
            }
            self.security_validation = Some((event.clone(), bound));
            Ok(SecurityReceipt { event, bound })
        })();
        if result.is_err() {
            self.fail();
        }
        result
    }
    fn security_candidate(&self, event: &SecurityEvent) -> Result<(), SecurityCommitError> {
        let mut log = self.log.clone();
        let mut writer = Sequencer::continuing(&log, self.epoch.0, 1_000);
        log.append(writer.append(
            EventKind::Security {
                event: Box::new(event.clone()),
            },
            log.store_version(),
        ))
        .map_err(|_| SecurityCommitError::Boundary)?;
        let mut candidate = self.typed.clone();
        Reducer::new(IDLENESS_WINDOW)
            .fold_from(&mut candidate, &log)
            .map_err(SecurityCommitError::Replay)?;
        is_legal(&candidate, &self.contract).map_err(|_| SecurityCommitError::Boundary)?;
        Ok(())
    }
    pub fn commit_security(
        &mut self,
        parent: u64,
        clock: &FencingClock,
        receipt: SecurityReceipt,
        ports: &dyn SecurityPorts,
    ) -> Result<CommitOutcome, SecurityCommitError> {
        let result = (|| {
            self.authenticate_security(&receipt.event, ports)?;
            if clock.latest() > self.epoch.0
                || self.security_validation.as_ref()
                    != Some(&(receipt.event.clone(), receipt.bound))
            {
                return Err(SecurityCommitError::Boundary);
            }
            self.security_candidate(&receipt.event)?;
            self.commit_checked(
                parent,
                Some(EventKind::Security {
                    event: Box::new(receipt.event),
                }),
                receipt.bound,
            )
            .map_err(SecurityCommitError::Transaction)
        })();
        self.security_validation = None;
        if result.is_err() {
            self.fail();
        }
        result
    }
    fn authenticate_security(
        &self,
        event: &SecurityEvent,
        ports: &dyn SecurityPorts,
    ) -> Result<(), SecurityCommitError> {
        if self
            .current
            .as_ref()
            .is_none_or(|t| t.op != event.operation())
            || self.proposed_by.is_none()
        {
            return Err(SecurityCommitError::Boundary);
        }
        let valid = match event {
            SecurityEvent::Pin(p) => ports.principal_authenticated(&p.owner),
            SecurityEvent::Unpin { owner, .. } => ports.principal_authenticated(owner),
            SecurityEvent::Expire { .. } => true,
            SecurityEvent::EscalatePending(e) => {
                ports.principal_authenticated(&e.operator) && ports.escalation_authorized(e)
            }
            SecurityEvent::EscalateComplete { id, operator } => {
                ports.principal_authenticated(operator)
                    && self
                        .security()
                        .escalations()
                        .get(id)
                        .is_some_and(|e| ports.escalation_authorized(e))
            }
            SecurityEvent::Erase(e) => ports.erasure_completed(e),
        };
        if valid {
            Ok(())
        } else {
            Err(SecurityCommitError::Boundary)
        }
    }
}
