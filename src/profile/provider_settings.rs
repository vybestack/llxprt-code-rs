use super::{anthropic, chat, codex, openai_responses, EphemeralSettings, ModelParams};
use crate::model_api::settings::{
    AnthropicSettingsDraft, CodexResponsesSettingsDraft, OpenAiResponsesSettingsDraft,
};
use crate::model_api::target::{ModelApi, ModelTarget, ProviderId};

pub(super) struct ParsedProviderSettings {
    pub(super) ephemeral: EphemeralSettings,
    pub(super) model_params: ModelParams,
    pub(super) anthropic_settings: Option<AnthropicSettingsDraft>,
    pub(super) codex_settings: Option<CodexResponsesSettingsDraft>,
    pub(super) openai_responses_settings: Option<OpenAiResponsesSettingsDraft>,
    pub(super) chat_missing_discriminator: Option<String>,
}

pub(super) fn parse(
    obj: &serde_json::Map<String, serde_json::Value>,
    name: &str,
    model: &str,
    provider_id: ProviderId,
    target: &ModelTarget,
) -> Result<ParsedProviderSettings, String> {
    if provider_id == ProviderId::Codex {
        let parsed = codex::parse(obj, name, model.to_string())?;
        Ok(ParsedProviderSettings {
            ephemeral: parsed.ephemeral,
            model_params: parsed.model_params,
            anthropic_settings: None,
            codex_settings: Some(parsed.draft),
            openai_responses_settings: None,
            chat_missing_discriminator: None,
        })
    } else if provider_id == ProviderId::Anthropic {
        let parsed = anthropic::parse(obj, name)?;
        Ok(ParsedProviderSettings {
            ephemeral: parsed.ephemeral,
            model_params: parsed.model_params,
            anthropic_settings: Some(parsed.draft),
            codex_settings: None,
            openai_responses_settings: None,
            chat_missing_discriminator: parsed.chat_missing_discriminator,
        })
    } else if target.api == ModelApi::Responses {
        let parsed = openai_responses::parse(obj, name)?;
        Ok(ParsedProviderSettings {
            ephemeral: parsed.ephemeral,
            model_params: parsed.model_params,
            anthropic_settings: None,
            codex_settings: None,
            openai_responses_settings: Some(parsed.draft),
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
