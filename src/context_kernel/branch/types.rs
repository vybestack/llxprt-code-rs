use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub type Namespace = String;
/// Harness declarations, never inferred from output text or an idempotence flag.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolContract {
    pub name: String,
    pub read_only: bool,
    pub reads: BTreeSet<String>,
    pub writes: BTreeSet<String>,
}
impl ToolContract {
    pub fn permits_branch(&self) -> bool {
        self.read_only
            && self.writes.is_empty()
            && matches!(
                self.name.as_str(),
                "read_file" | "list_directory" | "search_file_content"
            )
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub depth: u32,
    pub concurrency: usize,
    pub context: u64,
    pub management: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            depth: 4,
            concurrency: 4,
            context: 65_536,
            management: 1_000_000,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spawn {
    pub namespace: Namespace,
    pub parent: Option<Namespace>,
    pub parent_version: u64,
    pub scope: u64,
    /// Exact item identities/digests replicated, not model-reported rule text.
    pub constitutional: BTreeMap<String, String>,
    pub notes: BTreeSet<String>,
    pub limits: Limits,
    pub tools: Vec<ToolContract>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Round {
    pub call: String,
    pub tool: String,
    /// Complete declared read set at dispatch, including empty domains at version 0.
    pub reads: BTreeMap<String, u64>,
    pub store_handle: String,
    pub context_spend: u64,
    pub management_spend: u64,
    pub complete: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReturnStatus {
    CurrentEvidence,
    SuggestionNeedsRevalidation,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Returned {
    pub handle: String,
    pub digest: String,
    pub status: ReturnStatus,
    /// Always environment/evidential; no promotion field exists.
    pub revalidate: BTreeSet<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Branch {
    pub spawn: Spawn,
    pub depth: u32,
    pub rounds: Vec<Round>,
    pub reads: BTreeMap<String, u64>,
    pub context_spend: u64,
    pub management_spend: u64,
    pub returned: Option<Returned>,
    pub aborted: bool,
}
impl Branch {
    pub fn active(&self) -> bool {
        !self.aborted && self.returned.is_none()
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BranchEvent {
    Open(Spawn),
    Round {
        namespace: Namespace,
        round: Round,
    },
    Return {
        namespace: Namespace,
        returned: Returned,
    },
    Abort {
        namespace: Namespace,
    },
}
impl BranchEvent {
    pub fn operation(&self) -> &'static str {
        match self {
            Self::Open(_) => "branch-open",
            Self::Return { .. } => "branch-return",
            Self::Round { .. } => "admit-observation",
            Self::Abort { .. } => "branch-abort",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BranchError {
    #[error("branch namespace absent, reused, or inactive")]
    Namespace,
    #[error("branch constitutional replication failed")]
    Constitution,
    #[error("branch depth, concurrency, context, or management allotment exceeded")]
    Limit,
    #[error("branch tool is not an approved read-only class")]
    Tool,
    #[error("branch read-set, derivation, or return receipt mismatch")]
    Receipt,
}
