//! Phase-4 policy plane: proposal-only policy over the context store.
//!
//! Every component here *proposes*. None of them mutates store or executor
//! state. The plane covers classed queues with reserved service shares, the
//! admission governor, dual-threshold pressure, the fixed reclamation ladder,
//! the read-only runtime monitor, the rewrite journal (cache), the parameter
//! registry, and the progress macrostep measure.

pub mod cache;
pub mod governor;
pub mod ladder;
pub mod monitor;
pub mod params;
pub mod pressure;
pub mod progress;
pub mod queue;

#[cfg(test)]
mod tests;
