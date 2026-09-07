//! Checked-in model parameter registry (issue 64).
//!
//! The `known-model` acceptance mode checks a profile's unrecognized `modelParams`
//! against this registry at load. It is the pinned source of truth for which
//! parameter keys each provider's chat wire is allowed to carry, so a profile is
//! honest with the wire even when this build does not type every key.
//!
//! The parser (`crate::profile`) stays neutral and lossless: unrecognized keys land
//! in [`crate::profile::ModelParams::forwarded`] and every recognized-but-not-wire
//! name lands in [`crate::profile::ModelParams::unsupported`]. The acceptance
//! policy owned by `model_api` compares those names against this registry.

use crate::target::ProviderId;

/// The provider-wide chat keys this build maps to the `ModelSettings` ambient type.
pub(crate) const TYPED_CHAT_KEYS: &[&str] = &[
    "temperature",
    "top_p",
    "seed",
    "frequency_penalty",
    "presence_penalty",
];

/// Per-provider overrides: keys a provider's chat wire carries that this build does
/// not type but the checked-in registry documents. A name is queried through
/// [`known_for`], which respects the provider the resolved target selected.
const PROVIDER_OVERRIDES: &[(ProviderId, &[&str])] = &[
    // Anthropic Messages carries top_k and stop_sequences on the wire.
    (
        ProviderId::Anthropic,
        &["strict_temperature", "stop_sequences"],
    ),
    // The OpenAI Chat wire carries these sampling keys natively even though the shared
    // parser leaves the aliases out of its typed set.
    (ProviderId::OpenAi, &["logprobs", "top_logprobs"]),
    (ProviderId::OpenAiResponses, &["logprobs"]),
    (ProviderId::OpenAiVercel, &[]),
    (ProviderId::OpenAiCompatible, &[]),
    (ProviderId::Codex, &[]),
];

/// Whether a `modelParams` name is known for the effective provider.
///
/// The typed shared keys are always known; per-provider overrides extend that set.
pub(crate) fn known_for(provider: ProviderId, name: &str) -> bool {
    if TYPED_CHAT_KEYS.contains(&name) {
        return true;
    }
    PROVIDER_OVERRIDES
        .iter()
        .find(|(p, _)| *p == provider)
        .is_some_and(|(_, keys)| keys.contains(&name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_keys_are_known_everywhere() {
        for name in TYPED_CHAT_KEYS {
            assert!(known_for(ProviderId::OpenAi, name));
            assert!(known_for(ProviderId::Anthropic, name));
        }
    }

    #[test]
    fn overrides_are_provider_specific() {
        assert!(known_for(ProviderId::Anthropic, "stop_sequences"));
        assert!(known_for(ProviderId::OpenAi, "top_logprobs"));
        assert!(!known_for(ProviderId::OpenAi, "stop_sequences"));
    }

    #[test]
    fn unrelated_names_are_not_known() {
        assert!(!known_for(ProviderId::Anthropic, "definitely_not_a_param"));
        assert!(!known_for(ProviderId::OpenAi, "bogus"));
    }
}
