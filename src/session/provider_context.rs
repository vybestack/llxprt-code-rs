//! Durable publication of the actual pre-send context generation, not a synthetic
//! event used to make an eval pass. Publication failure refuses advancement.
use super::*;
impl SessionStore {
    pub(crate) fn record_provider_boundary(&self) -> Result<(), StoreError> {
        let mut guard = self
            .context
            .lock()
            .map_err(|_| StoreError::Lock("context mutex poisoned".into()))?;
        if guard.is_none() {
            *guard = Some(
                super::context_persist::recover_context_state(self).map_err(StoreError::Invalid)?,
            );
        }
        super::context_persist::persist_context(
            self,
            guard
                .as_ref()
                .ok_or_else(|| StoreError::Invalid("context state unavailable".into()))?,
        )
        .map_err(StoreError::Invalid)
    }
}
