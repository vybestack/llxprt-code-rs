//! Authenticated pin/escalation lifecycles and erasure overlay metadata.
use super::{valid_hash, SecurityError};
use crate::context_kernel::{
    canonical::Sink,
    ledger::{Authority, Origin, Source},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const PIN_CAP: usize = 32;
pub const PIN_TTL: u64 = 32;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pin {
    pub item: u64,
    pub owner: Source,
    pub expires: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Escalation {
    pub id: String,
    pub operator: Source,
    pub before: u64,
    pub after: u64,
    pub envelope_ceiling: u64,
    pub complete: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Erasure {
    pub leaf: String,
    pub vault_handle: String,
    pub post_checkpoint: String,
    pub pre_hashes_unverifiable: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SecurityEvent {
    Pin(Pin),
    Unpin { item: u64, owner: Source },
    Expire { sequence: u64 },
    EscalatePending(Escalation),
    EscalateComplete { id: String, operator: Source },
    Erase(Erasure),
}
impl SecurityEvent {
    pub fn operation(&self) -> &'static str {
        match self {
            Self::Pin(_) => "pin",
            Self::Unpin { .. } => "unpin",
            Self::Expire { .. } => "expire-pin",
            Self::EscalatePending(_) => "escalate-pending",
            Self::EscalateComplete { .. } => "escalate-complete",
            Self::Erase(_) => "redact",
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SecurityState {
    pins: BTreeMap<u64, Pin>,
    escalations: BTreeMap<String, Escalation>,
    erased: BTreeMap<String, Erasure>,
}
impl SecurityState {
    pub(crate) fn revoke_item(&mut self, item: u64) {
        self.pins.remove(&item);
    }
    pub fn pins(&self) -> &BTreeMap<u64, Pin> {
        &self.pins
    }
    pub fn escalations(&self) -> &BTreeMap<String, Escalation> {
        &self.escalations
    }
    pub fn erased(&self) -> &BTreeMap<String, Erasure> {
        &self.erased
    }
    pub fn encode(&self, sink: &mut Sink) {
        sink.tag("context-security");
        sink.blob(
            &serde_json::to_vec(&(&self.pins, &self.escalations, &self.erased))
                .expect("typed security state"),
        );
    }
    fn pending(&mut self, request: &Escalation) -> Result<(), SecurityError> {
        if request.id.is_empty()
            || self.escalations.len() >= 256
            || self.escalations.contains_key(&request.id)
            || request.complete
            || !operator(&request.operator)
            || request.after <= request.before
            || request.after > request.envelope_ceiling
        {
            return Err(SecurityError::Escalation);
        }
        self.escalations.insert(request.id.clone(), request.clone());
        Ok(())
    }
    pub(crate) fn apply(
        &mut self,
        event: &SecurityEvent,
        sequence: u64,
        placed: &BTreeSet<u64>,
    ) -> Result<(), SecurityError> {
        let mut next = self.clone();
        match event {
            SecurityEvent::Pin(pin) => {
                if next.pins.len() >= PIN_CAP
                    || next.pins.contains_key(&pin.item)
                    || !placed.contains(&pin.item)
                    || !principal(&pin.owner)
                    || pin.expires <= sequence
                    || pin.expires > sequence.saturating_add(PIN_TTL)
                {
                    return Err(SecurityError::Pin);
                }
                next.pins.insert(pin.item, pin.clone());
            }
            SecurityEvent::Unpin { item, owner } => {
                if !next.pins.get(item).is_some_and(|p| &p.owner == owner) {
                    return Err(SecurityError::Pin);
                }
                next.pins.remove(item);
            }
            SecurityEvent::Expire { sequence: expiry } => {
                if *expiry > sequence {
                    return Err(SecurityError::Pin);
                }
                next.pins.retain(|_, p| p.expires > sequence);
            }
            SecurityEvent::EscalatePending(request) => {
                next.pending(request)?;
            }
            SecurityEvent::EscalateComplete { id, operator } => {
                let request = next
                    .escalations
                    .get_mut(id)
                    .ok_or(SecurityError::Escalation)?;
                if request.complete || &request.operator != operator {
                    return Err(SecurityError::Escalation);
                }
                request.complete = true;
            }
            SecurityEvent::Erase(erasure) => {
                if erasure.leaf.is_empty()
                    || erasure.vault_handle.is_empty()
                    || !valid_hash(&erasure.post_checkpoint)
                    || !erasure.pre_hashes_unverifiable
                    || next.erased.contains_key(&erasure.leaf)
                {
                    return Err(SecurityError::Erasure);
                }
                next.erased.insert(erasure.leaf.clone(), erasure.clone());
            }
        }
        *self = next;
        Ok(())
    }
}
fn principal(source: &Source) -> bool {
    source.origin == Origin::Ingress
        && source.authority >= Authority::Agent
        && !source.leaf.is_empty()
}
fn operator(source: &Source) -> bool {
    principal(source) && source.authority == Authority::Operator
}
