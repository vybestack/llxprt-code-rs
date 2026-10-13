use crate::context_kernel::ledger::{Authority, Origin, Source};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Safety invariant, not a compactor-controlled tuning parameter.
pub const THETA: u16 = 9500;
pub const MAX_GRAPH_NODES: usize = 4096;
pub const MAX_ASSERTIONS: usize = 64;
pub const MAX_ARTIFACT_BYTES: usize = 65_536;
pub const MAX_ENVELOPE_BYTES: usize = MAX_ARTIFACT_BYTES * 8;
/// Worst case: two extractors plus every assertion scored against every leaf
/// with cap=1. This intentionally conservative floor cannot be underfunded.
pub const GATE_SPEND_FLOOR: u64 = (2 + MAX_ASSERTIONS as u64 * MAX_GRAPH_NODES as u64)
    * (MAX_ENVELOPE_BYTES as u64 + MAX_ARTIFACT_BYTES as u64);

/// Exact tuple: qualifiers travel with the statement, never as scattered tokens.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BoundFact {
    pub statement: String,
    pub subject: String,
    pub predicate: String,
    pub scope: u64,
    pub command: String,
    pub negated: bool,
    pub temporal: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub artifact: String,
    /// Exact serialized model-visible post-state supplied by the render owner.
    pub rendered_text: String,
    /// The rendered post-state, not durable-but-unrendered notes.
    pub rendered: Vec<BoundFact>,
    pub roots: Vec<Pointer>,
    pub graph: BTreeMap<String, Vec<Pointer>>,
    pub pending_notes: BTreeSet<String>,
    pub scope: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pointer {
    Ingress(String),
    Generated(String),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProvenanceFailure {
    Cycle,
    UngroundedRoot,
}
/// Closed semantic result. Infrastructure errors cannot masquerade as a pass.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    FailCompleteness { missing: Vec<BoundFact> },
    FailPrecision { unsupported: Vec<String> },
    FailProvenance(ProvenanceFailure),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationRecord {
    pub candidate_digest: String,
    pub verdict: Verdict,
    pub support: BTreeMap<String, BTreeSet<String>>,
    pub cap_pressure: bool,
    pub refreshed: usize,
    pub required_refinement_unavailable: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Leaf {
    pub id: String,
    pub source: Source,
    pub sanitized: String,
}
impl Leaf {
    pub fn is_ingress(&self) -> bool {
        self.source.origin == Origin::Ingress && self.source.leaf == self.id
    }
    pub fn authority(&self) -> Authority {
        self.source.authority
    }
}
/// Store/authentication owner returns authenticated, sanitized leaves only.
/// Generated receipts and notes must return None, even if their bytes match.
pub trait LeafStore {
    fn authenticated_leaf(&self, id: &str) -> Option<Leaf>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Score {
    pub entailment: u16,
    pub contradiction: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GateError {
    #[error("validation consumer unavailable: {0}")]
    Consumer(&'static str),
    #[error("invalid or oversized validation input/output")]
    Bounds,
    #[error("compactor and scorer configuration must be independent")]
    Independence,
}
/// Gate owns these prompts/ports. Execution and intent accounting live outside it.
pub trait GateConsumers {
    fn independence(&self) -> Independence;
    fn required_facts(&mut self, baseline: &[BoundFact]) -> Result<Vec<BoundFact>, GateError>;
    fn assertions(&mut self, artifact: &str) -> Result<Vec<String>, GateError>;
    fn score(
        &mut self,
        assertion: &str,
        leaves: &[Leaf],
        contradictions: &[BoundFact],
    ) -> Result<Score, GateError>;
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Independence {
    pub compactor: String,
    pub scorer: String,
}
/// Split escalation responses; no failure class authorizes a semantic commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Response {
    Commit,
    RestoreRequired,
    RegenerateFromIngress,
    RepairProvenance,
    DeterministicEmergency,
}
impl Verdict {
    pub fn response(&self) -> Response {
        match self {
            Self::Pass => Response::Commit,
            Self::FailCompleteness { .. } => Response::RestoreRequired,
            Self::FailPrecision { .. } => Response::RegenerateFromIngress,
            Self::FailProvenance(_) => Response::RepairProvenance,
        }
    }
}
pub fn candidate_digest(candidate: &Candidate) -> String {
    use sha2::{Digest, Sha256};
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(candidate).expect("typed candidate"))
    )
}
