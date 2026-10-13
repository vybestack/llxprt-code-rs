//! Phase 6 oracle. Support and authority are separate; intermediates never support.
mod gate;
mod provenance;
mod state;
mod types;
pub use gate::{deterministic_required, Gate};
pub use state::*;
pub use types::*;
