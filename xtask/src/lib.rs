//! The llxprt-code-rs offline quality and release gates: source analysis plus the
//! coordinating lint, fixture, source-bundle, and release commands.
//!
//! The LOC and complexity gates measure exactly the root crate's `src/**/*.rs`
//! (including library and binary sources), with no baselines, allow-lists, or
//! suppressions. The standalone coupling gate has a different scope: it discovers
//! public and private top-level modules from `src/lib.rs`, recursively scans each
//! module's production Rust files, and enforces a checked-in burn-down debt ledger.

pub mod analyze;
pub mod complexity;
pub mod coupling;
mod coupling_graph;
pub mod loc;
pub mod release;

pub use analyze::{find_production_sources, run_gate, Gate, Report, Violation};

// Re-export the fixed limits inherited from the sibling `llxprt-code` project.
pub const FILE_LOC_LIMIT: usize = 800;
pub const FUNCTION_LOC_LIMIT: usize = 80;
pub const CYCLOMATIC_LIMIT: usize = 25;
pub const COGNITIVE_LIMIT: usize = 30;
