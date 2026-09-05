//! Context transactions: issue #40, phase 3 red step.
//!
//! Responsibilities of this module (mirrors `context_kernel` layout):
//! * [`operation`] - the *closed* operation registry: one row per operation in
//!   design.tex §9 `tab:ops`, with proposer, flags (reclamation class,
//!   deterministic, rebase-safe) and the owning phase. Rows owned by later
//!   phases are present but gated, never omitted.
//! * [`executor`] - single-writer fenced transaction executor skeleton:
//!   `proposed -> snapshotted -> generated -> validated -> committed | aborted`
//!   with the universal commit preconditions of design §8.3-8.4 called
//!   centrally, compare-and-commit against the named parent version, and the
//!   `capability_not_landed` verdict for rows not yet owned.
//! * [`budget`] - budget accounting port and the pure predicates backing the
//!   universal preconditions (fit bound, protection floor, reclamation bar).

pub mod bound_port;
pub mod budget;
pub mod executor;
pub mod operation;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_registry;
#[cfg(test)]
mod tests_rows;
