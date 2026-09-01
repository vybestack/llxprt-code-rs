//! Context kernel for issue #38: the conversation is a typed, replayable structure.
//!
//! The kernel is split along its responsibilities: a canonical byte encoding
//! ([`canonical`]), an append-only event log with a total order and a checksum chain
//! ([`events`]), lane policies ([`lanes`]), scope lifecycle ([`scopes`]), the
//! conversation intermediate representation ([`ir`]), a deterministic reducer
//! ([`reducer`]), a table-driven legality gate ([`legality`]), and context-store
//! migration framing ([`migration`]). Rendering is never an input to structure: the
//! projection is derived from typed state, so replaying a recorded event prefix
//! yields byte-identical state and an identical hash.

pub mod canonical;
pub mod events;
pub mod ir;
pub mod lanes;
pub mod legality;
pub mod migration;
pub mod reducer;
pub mod scopes;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod phase2_tests;
