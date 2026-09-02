//! Phase 2 ingress pipeline (issue #39): everything external or generated enters here.
//!
//! The pipeline is one fail-closed transaction: a bounded volatile capture buffer, a
//! system-authority redactor, deterministic segmentation with structural classification,
//! and a single exempt durable write (the sanitized append). Quarantined plaintext goes
//! to the encrypted vault and only a vault reference reaches the spine. The pre-entry
//! filter is deliberately outside the transaction: it is rule-based, per tool, sees only
//! evidential items, and digests rather than deletes.

pub mod capture;
pub mod filter;
pub mod ingress;
pub mod launder;
pub mod redactor;
pub mod segment;

#[cfg(test)]
mod tests;
