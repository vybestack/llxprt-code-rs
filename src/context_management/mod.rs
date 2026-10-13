//! Phase 6 management execution: proposal-only data, one budget, six consumers.
//! The gateway port is implemented by the Phase 7 universal egress owner; this
//! boundary never opens a connection or duplicates the existing compactor.
mod budget;
mod execution;
mod gate_calls;
mod types;
pub use budget::{CandidatePermit, SpendBudget, SpendConfig};
pub use execution::{DerivationData, ManagementPlane};
pub use gate_calls::GateCalls;
pub use types::*;
