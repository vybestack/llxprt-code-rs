//! Independent, read-only shadow replay at the commit checkpoint (issue #46).
//!
//! The auditor never writes governed state. Log/reducer checks, projection legality,
//! and byte-identical replay are checked independently of the executor's cached state.
//! Budget, governor and progress receipts are separate checkpoints: a log alone does
//! not contain the inputs needed to invent those measurements.
use super::operation;
use crate::context_kernel::{
    events::{EventKind, EventLog, LogError},
    legality::{is_legal, RenderContract, Violation},
    reducer::{Reducer, ReducerError, TypedState, IDLENESS_WINDOW},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuditError {
    Log(LogError),
    Replay(ReducerError),
    Legality(Violation),
    Uncontracted { sequence: u64, operation: String },
    StateMismatch,
}

/// Exhaustive event-to-registry closure. Appends are the sanitized raw exemption;
/// provider turns are gateway observations, not operation commits.
pub fn operation_name(kind: &EventKind) -> Option<&str> {
    match kind {
        EventKind::Append { .. } | EventKind::ProviderTurn { .. } => None,
        EventKind::Semantic { event } => Some(event.operation()),
        EventKind::Branch { event } => Some(event.operation()),
        EventKind::Security { event } => Some(event.operation()),
        EventKind::Ledger { command } => Some(command.operation()),
        EventKind::OperationCommit { class, .. } => Some(class.name()),
    }
}

/// Audit a whole log, including each intermediate checkpoint, not only its tip.
pub fn audit_log(log: &EventLog, contract: &RenderContract) -> Result<usize, AuditError> {
    let reducer = Reducer::new(IDLENESS_WINDOW);
    let mut prefix = log.prefix(0);
    let mut state = reducer.fold(&prefix).map_err(AuditError::Replay)?;
    for event in log.events() {
        if let Some(name) = operation_name(&event.kind) {
            if operation::find(name).is_none() {
                return Err(AuditError::Uncontracted {
                    sequence: event.sequence,
                    operation: name.into(),
                });
            }
        }
        prefix.append(event.clone()).map_err(AuditError::Log)?;
        reducer
            .fold_from(&mut state, &prefix)
            .map_err(AuditError::Replay)?;
        if !matches!(event.kind, EventKind::Append { .. }) {
            is_legal(&state, contract).map_err(AuditError::Legality)?;
        }
        let replayed = reducer.fold(&prefix).map_err(AuditError::Replay)?;
        if replayed != state {
            return Err(AuditError::StateMismatch);
        }
    }
    Ok(prefix.len())
}

/// Live checkpoint: re-validate framing and replay the retained suffix, then compare all
/// state (including deduplication identities), not merely a caller-supplied hash.
pub fn audit_tip(
    log: &EventLog,
    state: &TypedState,
    contract: &RenderContract,
) -> Result<(), AuditError> {
    let mut verified = log.prefix(0);
    for event in log.events() {
        if let Some(name) = operation_name(&event.kind) {
            if operation::find(name).is_none() {
                return Err(AuditError::Uncontracted {
                    sequence: event.sequence,
                    operation: name.into(),
                });
            }
        }
        verified.append(event.clone()).map_err(AuditError::Log)?;
    }
    let replayed = Reducer::new(IDLENESS_WINDOW)
        .fold(&verified)
        .map_err(AuditError::Replay)?;
    if &replayed != state {
        return Err(AuditError::StateMismatch);
    }
    is_legal(&replayed, contract)
        .map(|_| ())
        .map_err(AuditError::Legality)
}
