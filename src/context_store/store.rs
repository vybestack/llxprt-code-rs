//! The store facade: modes, retrieval index, checkpoints, and the quiesce contract.

use crate::context_store::spine::{Spine, SpineError};
use crate::context_store::vault::{Vault, VaultError, VaultKey};
use std::ops::Range;

/// Explicit store mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreMode {
    Normal,
    ReadOnly,
    Unavailable,
}

impl StoreMode {
    /// Stable name for manifests and reports.
    pub fn name(self) -> &'static str {
        match self {
            StoreMode::Normal => "normal",
            StoreMode::ReadOnly => "read-only",
            StoreMode::Unavailable => "unavailable",
        }
    }

    /// Whether the mode admits writes.
    pub fn writable(self) -> bool {
        matches!(self, StoreMode::Normal)
    }
}

/// Why a state-advancing turn was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreBlocked {
    Mode { mode: &'static str },
}

/// One retrieval-index entry: a handle, the spine ranges it covers, and its labels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    pub handle: String,
    pub ranges: Vec<Range<u64>>,
    pub labels: Vec<String>,
}

/// A checkpoint: applied record count and spine length at checkpoint time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Checkpoint {
    pub applied: u64,
    pub spine_len: u64,
}

/// Errors raised by the store facade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    Spine(SpineError),
    Vault(VaultError),
    Blocked(StoreBlocked),
}

/// The external store: sanitized spine, encrypted vault, index, checkpoints, mode.
pub struct ContextStore {
    mode: StoreMode,
    spine: Spine,
    vault: Vault,
    index: Vec<IndexEntry>,
    checkpoint: Option<Checkpoint>,
}

impl ContextStore {
    /// Opens a store in normal mode with the given vault key.
    pub fn open(key: &VaultKey) -> Self {
        Self {
            mode: StoreMode::Normal,
            spine: Spine::new(),
            vault: Vault::open(key),
            index: Vec::new(),
            checkpoint: None,
        }
    }

    /// Current mode.
    pub fn mode(&self) -> StoreMode {
        self.mode
    }

    /// Sets the store mode; `store-mode` operation row.
    pub fn set_mode(&mut self, mode: StoreMode) {
        self.mode = mode;
    }

    /// Refuses a state-advancing turn or side effect in a non-writable mode.
    pub fn begin_state_advancing_turn(&self) -> Result<(), StoreBlocked> {
        if self.mode.writable() {
            Ok(())
        } else {
            Err(StoreBlocked::Mode {
                mode: self.mode.name(),
            })
        }
    }

    /// Appends sanitized bytes (the one exempt write) and indexes the handle.
    pub fn sanitized_append(
        &mut self,
        handle: &str,
        bytes: &[u8],
    ) -> Result<Range<u64>, StoreError> {
        self.begin_state_advancing_turn()
            .map_err(StoreError::Blocked)?;
        let range = self.spine.append(handle, bytes);
        self.index.push(IndexEntry {
            handle: handle.to_string(),
            ranges: vec![range.clone()],
            labels: Vec::new(),
        });
        Ok(range)
    }

    /// Seals quarantined plaintext into the vault.
    pub fn vault_put(&mut self, raw: &[u8], reason: &str) -> Result<String, StoreError> {
        self.begin_state_advancing_turn()
            .map_err(StoreError::Blocked)?;
        self.vault.put(raw, reason).map_err(StoreError::Vault)
    }

    /// Opens a vault handle.
    pub fn vault_get(&self, handle: &str) -> Result<Vec<u8>, StoreError> {
        self.vault.get(handle).map_err(StoreError::Vault)
    }

    /// Erases a vault handle, leaving a tombstone.
    pub fn vault_erase(&mut self, handle: &str) -> Result<(), StoreError> {
        self.vault.erase(handle).map_err(StoreError::Vault)
    }

    /// Bounded page read over the spine.
    pub fn read_page(
        &self,
        range: Range<u64>,
        limit: usize,
    ) -> Result<crate::context_store::spine::Page, StoreError> {
        self.spine
            .read_page(range, limit)
            .map_err(StoreError::Spine)
    }

    /// Rebuilds the retrieval index from the spine records; `index-rebuild` row.
    pub fn rebuild_index(&mut self) -> usize {
        let mut index = Vec::new();
        for record in self.spine.records() {
            index.push(IndexEntry {
                handle: record.handle.clone(),
                ranges: vec![record.range.clone()],
                labels: Vec::new(),
            });
        }
        let count = index.len();
        self.index = index;
        count
    }

    /// The retrieval index.
    pub fn index(&self) -> &[IndexEntry] {
        &self.index
    }

    /// Range selector: every index entry overlapping `range`.
    pub fn select(&self, range: Range<u64>) -> Vec<&IndexEntry> {
        self.index
            .iter()
            .filter(|entry| {
                entry
                    .ranges
                    .iter()
                    .any(|candidate| candidate.start < range.end && candidate.end > range.start)
            })
            .collect()
    }

    /// Writes a checkpoint at the current position.
    pub fn checkpoint(&mut self) -> Checkpoint {
        let checkpoint = Checkpoint {
            applied: self.spine.records().len() as u64,
            spine_len: self.spine.len(),
        };
        self.checkpoint = Some(checkpoint);
        checkpoint
    }

    /// Latest checkpoint.
    pub fn latest_checkpoint(&self) -> Option<Checkpoint> {
        self.checkpoint
    }

    /// Replays a tail of spine records after a checkpoint.
    pub fn replay_tail(
        &self,
        checkpoint: Checkpoint,
    ) -> Vec<crate::context_store::spine::SpineRecord> {
        // Records replay under the canonical positional names a fresh load
        // reconstructs, so a replayed tail is comparable across processes.
        self.spine
            .records()
            .iter()
            .enumerate()
            .skip(checkpoint.applied as usize)
            .map(|(index, record)| crate::context_store::spine::SpineRecord {
                handle: format!("sanitized-{index}"),
                ..record.clone()
            })
            .collect()
    }

    /// Encoded spine bytes.
    pub fn spine_bytes(&self) -> Vec<u8> {
        self.spine.encode()
    }

    /// Replaces the spine from encoded bytes, dropping any corrupt tail.
    pub fn load_spine(&mut self, encoded: &[u8]) -> usize {
        self.spine = Spine::load(encoded);
        self.rebuild_index();
        self.spine.recovered_tail_records()
    }

    /// Corrupt-tail count of the loaded spine.
    pub fn recovered_tail_records(&self) -> usize {
        self.spine.recovered_tail_records()
    }

    /// Deterministic serialized vault state for durable artifacts.
    pub fn vault_snapshot(&self) -> crate::context_store::vault::VaultSnapshot {
        self.vault.snapshot()
    }
}

/// Stable textual refusal used at the ingress seam.
fn store_refusal(error: &StoreError) -> String {
    match error {
        StoreError::Spine(_) => "spine refused the sanitized append".to_string(),
        StoreError::Vault(_) => "vault refused the quarantined write".to_string(),
        StoreError::Blocked(_) => "store mode refused the write".to_string(),
    }
}

impl crate::context_ingress::ingress::IngressSink for ContextStore {
    fn sanitized_append(&mut self, bytes: &[u8]) -> Result<Range<u64>, String> {
        let handle = format!(
            "ingress-{:016x}",
            crate::context_kernel::canonical::digest(bytes)
        );
        ContextStore::sanitized_append(self, &handle, bytes).map_err(|error| store_refusal(&error))
    }

    fn vault_put(&mut self, raw: &[u8], reason: &str) -> Result<String, String> {
        ContextStore::vault_put(self, raw, reason).map_err(|error| store_refusal(&error))
    }

    fn mode(&self) -> &'static str {
        self.mode().name()
    }
}
