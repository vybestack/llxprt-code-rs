//! Provider-neutral send admission and recovery. No network client lives here.
//! The session owns durability; callers consume affine send permits and never
//! interpret an unresolved intent as permission to repeat an attempt.

mod protocol;
mod render;
pub use protocol::*;
pub use render::*;
#[cfg(test)]
mod tests;
