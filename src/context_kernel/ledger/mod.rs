//! Phase 5 fact ledger. The reducer is the only writer; ports only propose.
//!
//! Recorded commands contain authenticated source and gate receipts. The ingress
//! and validation owners supply those receipts through `LedgerPorts`; a utility
//! score or gate pass alone never supplies authority. No text is parsed as an
//! authentication or adoption event. This boundary is staged for the later
//! session/render/security consumers, not a second session store or compactor.
mod admission;
mod freshness;
mod lifecycle;
mod port;
mod state;
mod types;

pub use port::{LedgerPorts, LedgerView, Proposal, RetirementRequest};
pub use state::Ledger;
pub use types::*;

#[cfg(test)]
pub(crate) mod tests;
#[cfg(test)]
mod tests_authority;
#[cfg(test)]
mod tests_lifecycle;
