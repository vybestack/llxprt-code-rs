//! The plain persisted record types of the session store: one round of the
//! turn loop, a recorded tool call, the branch lifecycle, one addressable
//! branch attempt, and the versioned state slot payload. Moved verbatim from
//! `src/session.rs` so the session module owns its records; `src/session.rs`
//! re-exports them so every existing path keeps compiling unchanged.

use crate::session::STORE_VERSION;
use serde::{Deserialize, Serialize};

/// One round of the turn loop: the assistant message (with its tool calls) and the
/// executed results. A final no-tool round has empty `calls`; this is the
/// assistant's final response and is always persisted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoundRecord {
    /// Assistant text emitted alongside the tool calls of this round.
    pub assistant: String,
    /// The tool calls made in this round (empty for the final response).
    pub calls: Vec<ToolCallRecord>,
}

/// A recorded tool call for a persisted tool transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    /// The provider-assigned tool call id (persisted and replayed verbatim).
    pub id: String,
    pub name: String,
    /// Valid argument-object JSON in the adapter's semantic representation. Providers preserve
    /// malformed raw strings only long enough for the host to reject them before execution.
    pub args: String,
    /// Whether the tool run reported success.
    pub ok: bool,
    /// True when the host refused to run the call (budget exhaustion);
    /// refused records never count as executed tool calls.
    #[serde(default)]
    pub refused: bool,
    pub result: String,
}

/// Lifecycle of one branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Pending,
    Completed,
    Failed,
}

/// A single addressable attempt at one turn. `parent_*` records the lineage this
/// branch continues: the branch it was forked from, plus that parent's turn and attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchRecord {
    /// Globally unique branch id, allocated from a checked counter.
    pub branch_id: String,
    /// 1-based turn number within the session.
    pub turn: u32,
    /// Attempt id within this turn (1 for the first attempt, 2+ for branches).
    pub attempt: u32,
    /// The branch this one continues (its parent lineage). `None` for the root branch.
    pub parent_branch: Option<String>,
    pub parent_turn: u32,
    pub parent_attempt: u32,
    /// The exact prompt that started this attempt.
    pub prompt: String,
    pub digest: String,
    /// Lifecycle state of this branch.
    pub lifecycle: Lifecycle,
    /// The complete round history, including the final no-tool response.
    #[serde(default)]
    pub rounds: Vec<RoundRecord>,
    /// Final plain-text summary (completed branches).
    #[serde(default)]
    pub summary: String,
    /// Terminal error description (failed branches).
    #[serde(default)]
    pub error: String,
    /// Unique owner token of the process that most recently held the reservation.
    #[serde(default)]
    pub owner: String,
    /// Wall-clock unix seconds when the reservation was made; zero once the
    /// branch reaches a terminal lifecycle.
    #[serde(default)]
    pub reserved_at: u64,
    /// Wall-clock unix seconds when the lease expires; past means stale. Zero
    /// once the branch reaches a terminal lifecycle: a completed or failed
    /// branch is no longer leased and cannot be reclaimed.
    #[serde(default)]
    pub lease_expiry: u64,
}

/// The versioned persistent payload stored inside each state slot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    pub version: u32,
    pub session_id: String,
    /// Canonical working directory pinned on the first turn.
    pub cwd: Option<String>,
    /// Filesystem device and inode of the atomically opened workspace root.
    #[serde(default)]
    pub cwd_dev: u64,
    #[serde(default)]
    pub cwd_ino: u64,
    #[serde(default)]
    pub branches: Vec<BranchRecord>,
    /// Source of unique branch ids (checked increments).
    #[serde(default)]
    pub next_branch_seq: u64,
}

impl SessionState {
    pub(crate) fn empty(session_id: &str) -> SessionState {
        SessionState {
            version: STORE_VERSION,
            session_id: session_id.to_string(),
            cwd: None,
            cwd_dev: 0,
            cwd_ino: 0,
            branches: Vec::new(),
            next_branch_seq: 0,
        }
    }
}
