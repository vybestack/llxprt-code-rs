//! Implements the Phase 6 proposal-only port with the same durable protocol.
//! The transport receives the exact quoted envelope, never conversation history.
use crate::context_gateway::*;
use crate::context_management::{CallIntent, ManagementError, ManagementGateway};
use crate::session::{ReservedRequest, SessionStore};

pub struct SessionManagementGateway<'a, F> {
    store: &'a SessionStore,
    reservation: &'a ReservedRequest,
    class: ProviderClass,
    transport: F,
    pending: Option<(CallIntent, SendPermit)>,
    sent: Option<(String, String, String)>,
}
impl<'a, F> SessionManagementGateway<'a, F>
where
    F: FnMut(&CallIntent) -> Result<String, ManagementError>,
{
    pub fn new(
        store: &'a SessionStore,
        reservation: &'a ReservedRequest,
        class: ProviderClass,
        transport: F,
    ) -> Self {
        Self {
            store,
            reservation,
            class,
            transport,
            pending: None,
            sent: None,
        }
    }
}
impl<F> ManagementGateway for SessionManagementGateway<'_, F>
where
    F: FnMut(&CallIntent) -> Result<String, ManagementError>,
{
    fn record_intent(&mut self, call: &CallIntent) -> Result<(), ManagementError> {
        if self.pending.is_some()
            || self.sent.is_some()
            || call.envelope.derivative_authority != "none"
        {
            return Err(ManagementError::Gateway);
        }
        let bytes = serde_json::to_vec(call).map_err(|_| ManagementError::Gateway)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > call.bound {
            return Err(ManagementError::Bounds);
        }
        let intent = SendIntent {
            identity: call.identity.clone(),
            branch: self.reservation.branch_id.clone(),
            owner: self.reservation.owner.clone(),
            epoch: call.epoch,
            context_version: call.turn,
            request_hash: digest(&bytes),
            contract_hash: digest(call.configuration.as_bytes()),
            class: self.class,
            kind: TurnKind::Management,
            allowance: usize::try_from(call.bound).map_err(|_| ManagementError::Bounds)?,
        };
        let permit = self
            .store
            .provider_intent(self.reservation, intent)
            .map_err(|_| ManagementError::Gateway)?;
        self.pending = Some((call.clone(), permit));
        Ok(())
    }
    fn send_once(&mut self, call: &CallIntent) -> Result<String, ManagementError> {
        let (expected, permit) = self.pending.take().ok_or(ManagementError::Gateway)?;
        if expected != *call {
            return Err(ManagementError::Gateway);
        }
        let intent = self
            .store
            .admit_send(self.reservation, permit)
            .map_err(|_| ManagementError::Gateway)?;
        match (self.transport)(call) {
            Ok(raw) => {
                self.sent = Some((call.identity.clone(), intent.identity, raw.clone()));
                Ok(raw)
            }
            Err(error) => {
                // Failed transport does not license another attempt under this identity.
                self.store
                    .provider_command(
                        self.reservation,
                        Command::Reject {
                            identity: intent.identity,
                        },
                    )
                    .map_err(|_| ManagementError::Gateway)?;
                Err(error)
            }
        }
    }
    fn admit_response(&mut self, call: &CallIntent, raw: &str) -> Result<(), ManagementError> {
        let (expected, identity, received) = self.sent.take().ok_or(ManagementError::Gateway)?;
        if expected != call.identity || received != raw {
            return Err(ManagementError::Gateway);
        }
        self.store
            .provider_command(
                self.reservation,
                Command::Stage {
                    identity: identity.clone(),
                    bytes: raw.as_bytes().to_vec(),
                },
            )
            .map_err(|_| ManagementError::Gateway)?;
        self.store
            .provider_command(
                self.reservation,
                Command::Admit {
                    identity,
                    executions: Vec::new(),
                },
            )
            .map_err(|_| ManagementError::Gateway)
    }
}
#[cfg(test)]
mod tests;
