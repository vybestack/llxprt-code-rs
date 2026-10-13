use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Identity is semantic and scoped, not a generated sentence hash.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FactId {
    pub subject: String,
    pub predicate: String,
    pub scope: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Authority {
    Environment,
    Agent,
    User,
    Operator,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Origin {
    Ingress,
    Generated,
}
/// Receipt obtained from authenticated ingress, not from generated citations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub leaf: String,
    pub authority: Authority,
    pub origin: Origin,
    pub scope: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FactClass {
    Obligation,
    Convenience,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FactKind {
    Normative,
    Empirical,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Freshness {
    Current,
    Stale,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvidenceStatus {
    Verified,
    Unverified,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Grammar {
    Obligation,
    Ambiguous,
    Evidence,
}
/// A normalized fixed-point [0,1] score. No NaN or platform float drift.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Utility(pub u16);
impl Utility {
    pub const ONE: u16 = 10_000;
    pub fn valid(self) -> bool {
        self.0 <= Self::ONE
    }
}
/// Logged utility inputs; all ages use the same declared normalization.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub accesses: u32,
    pub reuses: u32,
    pub age_windows: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerParams {
    pub version: u64,
    pub threshold: Utility,
    pub capacity: usize,
    pub unconfirmed_capacity: usize,
    pub protection_bytes: u64,
    pub decay_interval: u64,
    pub access_weight: u16,
    pub reuse_weight: u16,
    pub decay_weight: u16,
    pub consolidation_attempts: u32,
}
impl Default for LedgerParams {
    fn default() -> Self {
        Self {
            version: 1,
            threshold: Utility(5000),
            capacity: 256,
            unconfirmed_capacity: 8,
            protection_bytes: 65_536,
            decay_interval: 32,
            access_weight: 1000,
            reuse_weight: 2000,
            decay_weight: 500,
            consolidation_attempts: 3,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CarryingFold {
    pub text: String,
    pub depth: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: FactId,
    pub class: FactClass,
    pub kind: FactKind,
    pub source: Source,
    pub freshness: Freshness,
    pub evidence: EvidenceStatus,
    pub unconfirmed: bool,
    pub utility: Utility,
    pub usage: Usage,
    pub read_set: BTreeMap<String, u64>,
    pub premises: BTreeSet<FactId>,
    pub bindings: BTreeSet<String>,
    pub fold: CarryingFold,
    pub rendered: bool,
    pub retired: bool,
    pub last_decay: u64,
    pub superseded_by: Option<FactId>,
}
/// Gate receipt is bound to the exact fact and claim, directly to ingress leaves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateSupport {
    pub fact: FactId,
    pub claim: String,
    pub ingress_leaves: BTreeSet<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DischargeEvidence {
    Insufficient,
    Resolved(GateSupport),
    SupersededBy(FactId),
    ScopeClosed,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    pub source: Source,
    pub read_set: BTreeMap<String, u64>,
    pub supports: BTreeSet<FactId>,
}
/// Ordered conflict matrix: empirical invalidation never amends normative text.
/// Sanitized tool result and declared write set are committed as one event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolWrite {
    pub call_id: String,
    pub tool: String,
    pub scope: u64,
    pub sanitized: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Conflict {
    Normative {
        left: FactId,
        right: FactId,
    },
    Empirical {
        left: FactId,
        right: FactId,
    },
    PremiseInvalidated {
        normative: FactId,
        empirical: FactId,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Command {
    AdmitObligation {
        entry: Entry,
        grammar: Grammar,
    },
    AdmitConvenience {
        entry: Entry,
    },
    Demote {
        fact: FactId,
        reclaim: u64,
    },
    Discharge {
        fact: FactId,
        evidence: DischargeEvidence,
    },
    Consolidate {
        facts: BTreeSet<FactId>,
        composite: Entry,
        support: GateSupport,
        reclaim: u64,
    },
    Amend {
        fact: FactId,
        replacement: Entry,
        principal: Source,
    },
    Retract {
        fact: FactId,
        principal: Source,
    },
    Promote {
        fact: FactId,
        source: Source,
        adoption: Option<Source>,
        support: GateSupport,
    },
    FactInvalidate {
        result: ToolWrite,
        writes: BTreeMap<String, u64>,
    },
    Revalidate {
        observation: Observation,
    },
    StaleDemote {
        domains: BTreeSet<String>,
    },
    Decay {
        fact: FactId,
        usage: Usage,
    },
    Redact {
        leaf: String,
    },
}
impl Command {
    pub fn operation(&self) -> &'static str {
        match self {
            Self::AdmitObligation { .. } => "admit-obligation",
            Self::AdmitConvenience { .. } => "admit-convenience",
            Self::Demote { .. } => "demote",
            Self::Discharge { .. } => "discharge",
            Self::Consolidate { .. } => "consolidate-obligations",
            Self::Amend { .. } => "amend",
            Self::Retract { .. } => "retract",
            Self::Promote { .. } => "promote",
            Self::FactInvalidate { .. } => "fact-invalidate",
            Self::Revalidate { .. } => "revalidate",
            Self::StaleDemote { .. } => "stale-demote",
            Self::Decay { .. } => "decay",
            Self::Redact { .. } => "redact",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LedgerError {
    #[error("fact not found")]
    Missing,
    #[error("fact already exists or retired")]
    Identity,
    #[error("ledger authority denied")]
    Authority,
    #[error("environment obligation quarantined as evidence")]
    Quarantined,
    #[error("ledger overflow: consolidate, queue or escalate required")]
    Overflow,
    #[error("invalid ledger parameters or utility")]
    Parameters,
    #[error("ledger precondition failed")]
    Precondition,
    #[error("gate support absent or not bound to authenticated ingress")]
    Unsupported,
    #[error("discharge evidence insufficient")]
    Insufficient,
    #[error("obligations cannot decay, demote, amend or retract")]
    ObligationProtected,
}
