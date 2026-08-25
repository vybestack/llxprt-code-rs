//! The llxprt-code-rs offline quality gates: effective-LOC and cyclomatic/cognitive
//! complexity over the production source set, plus the `cargo xtask lint` coordinator.
//!
//! The production set is exactly the root crate's `src/**/*.rs` (every library and binary
//! source, including `src/bin/*`). Tests, `vendor/`, and the xtask itself live outside
//! that set by construction. There are no baselines, allow-lists, or suppressions: every
//! measured violation is reported.

pub mod analyze;
pub mod complexity;
pub mod loc;

pub use analyze::{find_production_sources, run_gate, Gate, Report, Violation};

// Re-export the thresholds the gates enforce. These are the fixed limits inherited from the
// sibling `llxprt-code` project (its ESLint guard enforces the same numbers):
// production file effective LOC <= 800, function effective LOC <= 80, cyclomatic <= 25,
// cognitive <= 30.
pub const FILE_LOC_LIMIT: usize = 800;
pub const FUNCTION_LOC_LIMIT: usize = 80;
pub const CYCLOMATIC_LIMIT: usize = 25;
pub const COGNITIVE_LIMIT: usize = 30;
