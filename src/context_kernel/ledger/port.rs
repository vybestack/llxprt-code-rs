use super::*;

/// Ingress/validation-owned authentication hooks. Implementations must bind
/// receipts to persisted ingress/adoption records, never generated text.
/// The default resolution evaluator refuses retirement.
pub trait LedgerPorts {
    fn authenticated(&self, source: &Source) -> bool;
    fn gate_supported(&self, support: &GateSupport) -> bool;
    /// Must match the authenticated harness call/result and its tool contract.
    fn tool_write_authenticated(
        &self,
        result: &ToolWrite,
        writes: &std::collections::BTreeMap<String, u64>,
    ) -> bool;
    fn discharge_evidence(&self, _obligation: &Entry, _candidate: &str) -> DischargeEvidence {
        DischargeEvidence::Insufficient
    }
}
/// Controller carries a justification, not a mutation capability.
pub struct RetirementRequest {
    pub fact: FactId,
    pub budget_justification: String,
}
/// A ledger-owned proposal carries no mutable state reference.
pub struct Proposal {
    pub command: Command,
}
pub trait LedgerView {
    fn ledger_active(&self, fact: &FactId) -> bool;
    fn request_retirement(
        &self,
        request: &RetirementRequest,
        ports: &dyn LedgerPorts,
    ) -> Option<Proposal>;
}
impl LedgerView for Ledger {
    fn ledger_active(&self, fact: &FactId) -> bool {
        self.entry(fact).is_some_and(|entry| self.active(entry))
    }
    fn request_retirement(
        &self,
        request: &RetirementRequest,
        ports: &dyn LedgerPorts,
    ) -> Option<Proposal> {
        let entry = self.entry(&request.fact)?;
        if entry.retired || request.budget_justification.is_empty() {
            return None;
        }
        let evidence = ports.discharge_evidence(entry, &request.budget_justification);
        if evidence == DischargeEvidence::Insufficient {
            return None;
        }
        Some(Proposal {
            command: Command::Discharge {
                fact: request.fact.clone(),
                evidence,
            },
        })
    }
}
impl Command {
    /// Checked at the executor boundary before a typed command is committed.
    pub fn authenticate(&self, ports: &dyn LedgerPorts) -> Result<(), LedgerError> {
        let valid = match self {
            Self::AdmitObligation { entry, .. } | Self::AdmitConvenience { entry } => {
                ports.authenticated(&entry.source)
            }
            Self::Amend {
                replacement,
                principal,
                ..
            } => ports.authenticated(principal) && ports.authenticated(&replacement.source),
            Self::Retract { principal, .. } => ports.authenticated(principal),
            Self::Promote {
                source,
                adoption,
                support,
                ..
            } => {
                ports.authenticated(source)
                    && adoption
                        .as_ref()
                        .is_none_or(|principal| ports.authenticated(principal))
                    && ports.gate_supported(support)
            }
            Self::Consolidate {
                composite, support, ..
            } => ports.authenticated(&composite.source) && ports.gate_supported(support),
            Self::Discharge {
                evidence: DischargeEvidence::Resolved(support),
                ..
            } => !support.claim.is_empty() && ports.gate_supported(support),
            Self::Revalidate { observation } => ports.authenticated(&observation.source),
            Self::FactInvalidate { result, writes } => {
                ports.tool_write_authenticated(result, writes)
            }
            _ => true,
        };
        if valid {
            Ok(())
        } else {
            Err(LedgerError::Authority)
        }
    }
}
