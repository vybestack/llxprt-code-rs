//! Phase 2 external context store (issue #39): append-only sanitized spine, encrypted
//! vault, retrieval index, checkpoints, erasure tombstones, corrupt-tail recovery, and
//! explicit store modes. Read-only and unavailable modes block state-advancing turns and
//! side effects, which is the store side of the `quiesce-unwritable` outcome.

pub mod ops;
pub mod spine;
pub mod store;
pub mod vault;

mod tests;
