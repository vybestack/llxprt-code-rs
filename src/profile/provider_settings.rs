//! Provider-neutral carriers for the provider-scoped settings a profile parsed.
//!
//! The profile layer knows *spellings*: which dotted keys exist on disk, their types,
//! and which value sets are self-consistent. It does not know which provider or API
//! those settings end up on, nor how they map onto a backend's typed enums. Those are
//! provider-layer decisions, so each parser here returns one of the typed payloads
//! below and `model_api::interpret` maps it onto the backend's types.
//!
//! Every value here is validated: booleans are booleans, enum-like strings are
//! spellings that the on-disk contract documents, and bounded scalars are bounded.
//! Interpretation therefore never has to re-validate raw profile text; it only maps
//! a validated spelling onto the backend enum and refuses the combinations a backend
//! cannot honor (which is compatibility policy, not parsing).

use super::{anthropic, chat, codex, openai_responses, EphemeralSettings, ModelParams};
use crate::target::{ApiSelector, ProviderId};

/// The prompt-caching spelling shared by the Anthropic Messages and OpenAI Responses
/// on-disk keys. `off` disables caching; the retention spellings enable it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PromptCachingSetting {
    Off,
    Cached,
}

impl PromptCachingSetting {
    /// Parse the Anthropic Messages spelling: absent means cached, and the only
    /// accepted value is `off`.
    pub fn anthropic(raw: Option<&str>) -> Result<Self, String> {
        match raw {
            None | Some("off") => Ok(match raw {
                None => Self::Cached,
                Some(_) => Self::Off,
            }),
            Some(_) => Err("must be 'off'".to_string()),
        }
    }

    /// Parse the OpenAI Responses spelling: absent, `1h`, and `24h` mean cached,
    /// and `off` disables it.
    pub fn openai_responses(raw: Option<&str>) -> Result<Self, String> {
        match raw {
            None | Some("1h" | "24h") => Ok(Self::Cached),
            Some("off") => Ok(Self::Off),
            Some(_) => Err("must be off, 1h, or 24h".to_string()),
        }
    }
}

/// The Anthropic Messages provider-scoped settings carried by a profile.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AnthropicSettings {
    pub prompt_caching: Option<PromptCachingSetting>,
}

/// The OpenAI Responses provider-scoped settings carried by a profile.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpenAiResponsesSettings {
    pub reasoning_enabled: Option<bool>,
    pub reasoning_effort: Option<String>,
    pub reasoning_summary: Option<String>,
    pub text_verbosity: Option<String>,
    pub prompt_caching: Option<PromptCachingSetting>,
}

/// The Codex Responses provider-scoped settings carried by a profile.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodexResponsesSettings {
    pub reasoning_enabled: bool,
}

/// The provider-scoped parse outcome for one profile.
///
/// Exactly one of the three settings payloads is `Some`, chosen by the same
/// provider/API branch the historical parser used so class-4 diagnostics keep
/// their order.
pub(super) struct ParsedProviderSettings {
    pub(super) ephemeral: EphemeralSettings,
    pub(super) model_params: ModelParams,
    pub(super) anthropic_settings: Option<AnthropicSettings>,
    pub(super) codex_settings: Option<CodexResponsesSettings>,
    pub(super) openai_responses_settings: Option<OpenAiResponsesSettings>,
    pub(super) chat_missing_discriminator: Option<String>,
}

/// Dispatch to the provider-specific parse using the *neutral* selection.
///
/// The branch is spelled in terms of provider identity and the recorded API
/// selector, not a resolved backend target: the provider spellings that mean
/// "Responses" (`openai-responses`, `codex`, and an explicit `responses`
/// selector on `openai`) are the same set the historical resolver accepted, and
/// the compatibility refusals that `model_api` performs later are unchanged.
pub(super) fn parse(
    obj: &serde_json::Map<String, serde_json::Value>,
    name: &str,
    selection: &super::selection::ProfileSelection,
) -> Result<ParsedProviderSettings, String> {
    if selection.provider == ProviderId::Codex {
        let parsed = codex::parse(obj, name)?;
        Ok(ParsedProviderSettings {
            ephemeral: parsed.ephemeral,
            model_params: parsed.model_params,
            anthropic_settings: None,
            codex_settings: Some(parsed.settings),
            openai_responses_settings: None,
            chat_missing_discriminator: None,
        })
    } else if selection.provider == ProviderId::Anthropic {
        let parsed = anthropic::parse(obj, name)?;
        Ok(ParsedProviderSettings {
            ephemeral: parsed.ephemeral,
            model_params: parsed.model_params,
            anthropic_settings: Some(parsed.settings),
            codex_settings: None,
            openai_responses_settings: None,
            chat_missing_discriminator: parsed.chat_missing_discriminator,
        })
    } else if is_responses_selection(selection) {
        let parsed = openai_responses::parse(obj, name)?;
        Ok(ParsedProviderSettings {
            ephemeral: parsed.ephemeral,
            model_params: parsed.model_params,
            anthropic_settings: None,
            codex_settings: None,
            openai_responses_settings: Some(parsed.settings),
            chat_missing_discriminator: None,
        })
    } else {
        let (ephemeral, model_params, chat_missing_discriminator) = chat::parse_chat(obj, name)?;
        Ok(ParsedProviderSettings {
            ephemeral,
            model_params,
            anthropic_settings: None,
            codex_settings: None,
            openai_responses_settings: None,
            chat_missing_discriminator,
        })
    }
}

/// The set of neutral selections that parse with the OpenAI Responses settings
/// grammar. This mirrors the historical `target.api == ModelApi::Responses`
/// branch for every selection that reaches it: incompatible combinations are
/// refused by `model_api` after parsing, exactly as before.
fn is_responses_selection(selection: &super::selection::ProfileSelection) -> bool {
    match selection.provider {
        ProviderId::OpenAiResponses | ProviderId::Codex => true,
        ProviderId::OpenAi => selection.api == Some(ApiSelector::Responses),
        ProviderId::OpenAiVercel | ProviderId::OpenAiCompatible | ProviderId::Anthropic => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_caching_spellings_follow_the_on_disk_contract() {
        assert_eq!(
            PromptCachingSetting::anthropic(None),
            Ok(PromptCachingSetting::Cached)
        );
        assert_eq!(
            PromptCachingSetting::anthropic(Some("off")),
            Ok(PromptCachingSetting::Off)
        );
        assert!(PromptCachingSetting::anthropic(Some("1h")).is_err());
        assert_eq!(
            PromptCachingSetting::openai_responses(None),
            Ok(PromptCachingSetting::Cached)
        );
        assert_eq!(
            PromptCachingSetting::openai_responses(Some("24h")),
            Ok(PromptCachingSetting::Cached)
        );
        assert_eq!(
            PromptCachingSetting::openai_responses(Some("off")),
            Ok(PromptCachingSetting::Off)
        );
        assert!(PromptCachingSetting::openai_responses(Some("1week")).is_err());
    }
}
