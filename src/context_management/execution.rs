use super::*;
use crate::context_ingress::ingress::{IngressRecord, IngressSink, IngressTxn};
use crate::context_kernel::validation::MAX_ARTIFACT_BYTES;
use std::collections::BTreeMap;

/// Output is volatile, authority-free data until the existing ingestion owner admits it.
pub struct DerivationData {
    raw: String,
}
impl DerivationData {
    pub fn text(&self) -> &str {
        &self.raw
    }
    pub fn ingest(
        self,
        txn: &mut IngressTxn,
        sink: &mut dyn IngressSink,
    ) -> Result<Vec<IngressRecord>, ManagementError> {
        txn.capture_derivation("management", self.raw.as_bytes())
            .map_err(|_| ManagementError::Ingestion)?;
        txn.commit(sink).map_err(|_| ManagementError::Ingestion)
    }
}
pub struct ManagementPlane {
    spend: SpendBudget,
    epoch: u64,
    namespace: String,
    sequence: u64,
    sent_compactors: BTreeMap<u64, String>,
}
impl ManagementPlane {
    /// Namespace is gateway-issued and persisted; a new process must not reuse it.
    pub fn new(spend: SpendBudget, epoch: u64, namespace: String) -> Result<Self, ManagementError> {
        if epoch == 0 || namespace.is_empty() || namespace.len() > 256 {
            return Err(ManagementError::Configuration);
        }
        Ok(Self {
            spend,
            epoch,
            namespace,
            sequence: 0,
            sent_compactors: BTreeMap::new(),
        })
    }
    pub fn spend(&self) -> &SpendBudget {
        &self.spend
    }
    pub fn reserve_candidate(&mut self) -> Result<CandidatePermit, ManagementError> {
        self.spend.reserve_candidate()
    }
    pub fn finish_candidate(&mut self, permit: CandidatePermit) -> Result<(), ManagementError> {
        self.sent_compactors.remove(&permit.id);
        self.spend.finish_candidate(permit)
    }
    pub fn compactor_configuration(&self, permit: &CandidatePermit) -> Option<&str> {
        self.spend.check_permit(permit).ok()?;
        self.sent_compactors.get(&permit.id).map(String::as_str)
    }
    pub fn replenish(&mut self, turn: u64) -> Result<(), ManagementError> {
        self.spend.replenish(turn)
    }
    pub fn execute(
        &mut self,
        consumer: Consumer,
        configuration: &str,
        bound: u64,
        envelope: QuotedEnvelope,
        gateway: &mut dyn ManagementGateway,
    ) -> Result<DerivationData, ManagementError> {
        self.check_call(configuration, bound, &envelope)?;
        self.spend.charge(consumer, bound)?;
        self.dispatch(consumer, configuration, bound, envelope, gateway)
    }
    /// Compaction is delegated to the existing compactor through the same gateway port.
    pub fn compact(
        &mut self,
        permit: &CandidatePermit,
        configuration: &str,
        bound: u64,
        envelope: QuotedEnvelope,
        gateway: &mut dyn ManagementGateway,
    ) -> Result<DerivationData, ManagementError> {
        self.check_call(configuration, bound, &envelope)?;
        self.spend.check_permit(permit)?;
        if self.sent_compactors.contains_key(&permit.id) {
            return Err(ManagementError::Turn);
        }
        self.spend.charge_compactor(permit, bound)?;
        self.sent_compactors
            .insert(permit.id, configuration.to_owned());
        self.dispatch(Consumer::Compactor, configuration, bound, envelope, gateway)
    }
    pub(super) fn gate_call(
        &mut self,
        permit: &CandidatePermit,
        consumer: Consumer,
        configuration: &str,
        envelope: QuotedEnvelope,
        gateway: &mut dyn ManagementGateway,
    ) -> Result<DerivationData, ManagementError> {
        if !consumer.gate_only() {
            return Err(ManagementError::Configuration);
        }
        let bytes = serde_json::to_vec(&envelope)
            .map_err(|_| ManagementError::Bounds)?
            .len() as u64;
        // Conservative bound counts full quoted input and the bounded output allowance.
        let bound = bytes
            .checked_add(MAX_ARTIFACT_BYTES as u64)
            .ok_or(ManagementError::Bounds)?;
        self.check_call(configuration, bound, &envelope)?;
        self.spend.charge_gate(permit, bound)?;
        self.dispatch(consumer, configuration, bound, envelope, gateway)
    }
    fn check_call(
        &self,
        configuration: &str,
        bound: u64,
        envelope: &QuotedEnvelope,
    ) -> Result<(), ManagementError> {
        let minimum = serde_json::to_vec(envelope)
            .map_err(|_| ManagementError::Bounds)?
            .len() as u64
            + MAX_ARTIFACT_BYTES as u64;
        if configuration.is_empty()
            || bound < minimum
            || envelope.derivative_authority != "none"
            || envelope.instruction.is_empty()
            || serde_json::to_vec(envelope)
                .map_err(|_| ManagementError::Bounds)?
                .len()
                > crate::context_kernel::validation::MAX_ENVELOPE_BYTES
        {
            return Err(ManagementError::Bounds);
        }
        Ok(())
    }
    fn dispatch(
        &mut self,
        consumer: Consumer,
        configuration: &str,
        bound: u64,
        envelope: QuotedEnvelope,
        gateway: &mut dyn ManagementGateway,
    ) -> Result<DerivationData, ManagementError> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or(ManagementError::Bounds)?;
        let intent = CallIntent {
            identity: format!("{}:{}:{}", self.namespace, self.epoch, self.sequence),
            epoch: self.epoch,
            turn: self.spend.turn(),
            consumer,
            configuration: configuration.to_owned(),
            bound,
            envelope,
        };
        gateway.record_intent(&intent)?;
        let raw = gateway.send_once(&intent)?;
        if raw.is_empty() || raw.len() > MAX_ARTIFACT_BYTES {
            return Err(ManagementError::Bounds);
        }
        gateway.admit_response(&intent, &raw)?;
        Ok(DerivationData { raw })
    }
}
