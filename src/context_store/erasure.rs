//! Vault owner issues an erasure receipt over the actual tombstoned snapshot.
use super::store::{ContextStore, StoreError};
use crate::context_kernel::security::{hash, Erasure};
impl ContextStore {
    pub fn erase_with_receipt(&mut self, leaf: &str, handle: &str) -> Result<Erasure, StoreError> {
        self.vault_erase(handle)?;
        Ok(Erasure {
            leaf: leaf.into(),
            vault_handle: handle.into(),
            post_checkpoint: self.erasure_checkpoint(),
            pre_hashes_unverifiable: true,
        })
    }
    /// A receipt is bound to the entire post-erasure vault and sanitized prefix.
    /// Durable publishers must publish this snapshot before authorizing the overlay.
    pub fn erasure_matches(&self, receipt: &Erasure) -> bool {
        self.vault_snapshot()
            .slots
            .iter()
            .any(|s| s.handle == receipt.vault_handle && s.erased)
            && receipt.pre_hashes_unverifiable
            && !receipt.leaf.is_empty()
            && receipt.post_checkpoint == self.erasure_checkpoint()
    }
    fn erasure_checkpoint(&self) -> String {
        let snapshot = serde_json::to_vec(&self.vault_snapshot()).expect("typed vault snapshot");
        let mut sink = crate::context_kernel::canonical::Sink::new();
        sink.tag("post-erasure-checkpoint");
        sink.blob(&self.spine_bytes());
        sink.blob(&snapshot);
        hash(&sink.finish())
    }
}
