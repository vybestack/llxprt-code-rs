//! Provider/API resolution entry points for the registered targets.
//!
//! Identity types live in the neutral leaf `crate::target`; this module re-exports
//! them for the provider layer. The policy that used to live here -- which
//! provider/API pairs are registered, and the fixed refusal text a profile gets
//! for a pairing it cannot have -- moved to `crate::model_api::interpret`, which
//! owns interpretation of a parsed profile into a backend target.

pub(crate) use crate::target::{ModelApi, ModelTarget, ProviderId, TransportKind};
