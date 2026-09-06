//! Interpretation of a parsed, provider-neutral profile into backend data.
//!
//! This is the seam. `profile` records what the operator wrote; this module decides
//! which backend that names. Provider/API resolution and compatibility, the
//! construction of provider-specific settings drafts, and the mapping from documented
//! spellings onto backend enums all live here, on the provider side of the seam.

use crate::profile::provider_settings::{
    AnthropicSettings, CodexResponsesSettings, OpenAiResponsesSettings, PromptCachingSetting,
};
use crate::profile::Profile;
use crate::target::{ModelApi, ModelTarget, ProviderId};

use super::settings::{
    AnthropicSettingsDraft, CodexResponsesSettingsDraft, OpenAiResponsesSettingsDraft,
    PromptCaching,
};

/// A profile interpreted for backend construction: the resolved target plus the
/// provider-specific settings drafts.
///
/// At most one draft is `Some`, and which one is decided by the resolved target, so
/// no caller can read another provider's settings for a target.
#[derive(Debug)]
pub(crate) struct ResolvedProfile {
    pub(crate) target: ModelTarget,
    pub(crate) anthropic: Option<AnthropicSettingsDraft>,
    pub(crate) codex: Option<CodexResponsesSettingsDraft>,
    pub(crate) openai_responses: Option<OpenAiResponsesSettingsDraft>,
}

impl ResolvedProfile {
    /// Interpret a parsed profile.
    ///
    /// Resolution runs first so compatibility refusals keep their historical
    /// position -- before any settings interpretation, and with the same messages
    /// the profile contract has always produced.
    pub(crate) fn interpret(profile: &Profile) -> Result<Self, String> {
        let target = resolve_target(profile)?;
        let (anthropic, codex, openai_responses) = match (target.provider, target.api) {
            (ProviderId::Anthropic, _) => (
                Some(anthropic_draft(
                    profile
                        .anthropic_settings
                        .as_ref()
                        .ok_or("Anthropic Messages settings were not resolved")?,
                )),
                None,
                None,
            ),
            (ProviderId::Codex, _) => (
                None,
                Some(codex_draft(
                    profile
                        .codex_settings
                        .as_ref()
                        .ok_or("Codex Responses settings were not resolved")?,
                    &profile.model,
                )),
                None,
            ),
            (_, ModelApi::Responses) => (
                None,
                None,
                Some(openai_responses_draft(
                    profile
                        .openai_responses_settings
                        .as_ref()
                        .ok_or("OpenAI Responses settings were not resolved")?,
                )?),
            ),
            (_, ModelApi::ChatCompletions) | (_, ModelApi::AnthropicMessages) => (None, None, None),
        };
        Ok(Self {
            target,
            anthropic,
            codex,
            openai_responses,
        })
    }
}

/// Resolve the recorded provider and selector into a target, reusing the selector
/// spellings the profile already validated. The refusal matrix is the historical
/// one: the same provider/API pairs, the same message text.
fn resolve_target(profile: &Profile) -> Result<ModelTarget, String> {
    let api = match (profile.provider_selection, profile.api_selection) {
        (ProviderId::Anthropic, Some(selector)) => {
            return Err(unsupported_target(
                &profile.name,
                ProviderId::Anthropic,
                selector.api(),
            ));
        }
        (ProviderId::Anthropic, None) => ModelApi::AnthropicMessages,
        (ProviderId::OpenAi, Some(selector)) => selector.api(),
        (ProviderId::OpenAi, None) => ModelApi::ChatCompletions,
        (ProviderId::OpenAiResponses, Some(crate::target::ApiSelector::Responses) | None) => {
            ModelApi::Responses
        }
        (ProviderId::OpenAiResponses, Some(crate::target::ApiSelector::Chat)) => {
            return Err(unsupported_target(
                &profile.name,
                ProviderId::OpenAiResponses,
                ModelApi::ChatCompletions,
            ));
        }
        (
            ProviderId::OpenAiVercel | ProviderId::OpenAiCompatible,
            Some(crate::target::ApiSelector::Responses),
        ) => {
            return Err(unsupported_target(
                &profile.name,
                profile.provider_selection,
                ModelApi::Responses,
            ));
        }
        (
            ProviderId::OpenAiVercel | ProviderId::OpenAiCompatible,
            Some(crate::target::ApiSelector::Chat) | None,
        ) => ModelApi::ChatCompletions,
        (ProviderId::Codex, Some(crate::target::ApiSelector::Chat)) => {
            return Err(unsupported_target(
                &profile.name,
                ProviderId::Codex,
                ModelApi::ChatCompletions,
            ));
        }
        (ProviderId::Codex, Some(crate::target::ApiSelector::Responses) | None) => {
            ModelApi::Responses
        }
    };
    Ok(ModelTarget {
        provider: profile.provider_selection,
        api,
        transport: crate::target::TransportKind::Http,
    })
}

fn unsupported_target(profile_name: &str, provider: ProviderId, api: ModelApi) -> String {
    format!(
        "profile {profile_name:?}: provider {:?} does not support API {:?}",
        provider.as_str(),
        api.selector_name()
    )
}

fn anthropic_draft(settings: &AnthropicSettings) -> AnthropicSettingsDraft {
    AnthropicSettingsDraft {
        prompt_caching: match settings.prompt_caching {
            Some(PromptCachingSetting::Off) => PromptCaching::Off,
            // Absent means cached, matching the on-disk contract.
            Some(PromptCachingSetting::Cached) | None => PromptCaching::Cached,
        },
    }
}

fn codex_draft(settings: &CodexResponsesSettings, model: &str) -> CodexResponsesSettingsDraft {
    CodexResponsesSettingsDraft::new(model.to_string(), settings.reasoning_enabled)
}

fn openai_responses_draft(
    settings: &OpenAiResponsesSettings,
) -> Result<OpenAiResponsesSettingsDraft, String> {
    Ok(OpenAiResponsesSettingsDraft {
        reasoning_effort: settings
            .reasoning_effort
            .as_deref()
            .map(map_effort)
            .transpose()?,
        reasoning_summary: settings
            .reasoning_summary
            .as_deref()
            .map(map_summary)
            .transpose()?,
        text_verbosity: settings
            .text_verbosity
            .as_deref()
            .map(map_verbosity)
            .transpose()?,
        prompt_caching: match settings.prompt_caching {
            Some(PromptCachingSetting::Off) => PromptCaching::Off,
            // Absent, `1h`, and `24h` all mean cached.
            Some(PromptCachingSetting::Cached) | None => PromptCaching::Cached,
        },
    })
}

/// Map a documented effort spelling onto the backend enum. The profile validator
/// already rejected every other spelling, so an unmatched value here is a bug in
/// this crate's two tables agreeing -- not a profile error.
fn map_effort(value: &str) -> Result<serdes_ai::models::openai::ReasoningEffort, String> {
    match value {
        "low" => Ok(serdes_ai::models::openai::ReasoningEffort::Low),
        "medium" => Ok(serdes_ai::models::openai::ReasoningEffort::Medium),
        "high" => Ok(serdes_ai::models::openai::ReasoningEffort::High),
        other => Err(format!(
            "unmapped OpenAI Responses reasoning effort: {other}"
        )),
    }
}

fn map_summary(value: &str) -> Result<serdes_ai::models::openai::ReasoningSummary, String> {
    use serdes_ai::models::openai::ReasoningSummary;
    match value {
        "concise" => Ok(ReasoningSummary::Concise),
        "detailed" => Ok(ReasoningSummary::Detailed),
        "auto" => Ok(ReasoningSummary::Auto),
        other => Err(format!(
            "unmapped OpenAI Responses reasoning summary: {other}"
        )),
    }
}

fn map_verbosity(value: &str) -> Result<serdes_ai::models::openai::TextVerbosity, String> {
    use serdes_ai::models::openai::TextVerbosity;
    match value {
        "low" => Ok(TextVerbosity::Low),
        "medium" => Ok(TextVerbosity::Medium),
        "high" => Ok(TextVerbosity::High),
        other => Err(format!("unmapped OpenAI Responses text verbosity: {other}")),
    }
}
