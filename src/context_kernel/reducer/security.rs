//! Replay of authenticated security events; generic executor commits cannot mint them.
use super::*;
use crate::context_kernel::security::SecurityEvent;

pub(super) fn apply_security(
    state: &mut TypedState,
    event: &SecurityEvent,
    sequence: u64,
) -> Result<(), ReducerError> {
    let placed = state
        .conversation_ir
        .items()
        .iter()
        .filter(|i| i.region().is_some())
        .map(|i| i.id().value())
        .collect();
    state
        .security
        .apply(event, sequence, &placed)
        .map_err(ReducerError::Security)?;
    state.pins = state
        .security
        .pins()
        .keys()
        .map(|id| ItemId::new(*id))
        .collect();
    if let SecurityEvent::Erase(erasure) = event {
        state.semantic.erase(&erasure.leaf);
        state
            .ledger
            .apply(
                &crate::context_kernel::ledger::Command::Redact {
                    leaf: erasure.leaf.clone(),
                },
                sequence,
                &state.scope_registry,
            )
            .map_err(ReducerError::Ledger)?;
    }
    Ok(())
}
