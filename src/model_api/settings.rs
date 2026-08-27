use std::time::Duration;

use serdes_ai::models::chatgpt_oauth::{
    ChatGptOAuthRequestSettings, ChatGptPromptCacheKey, ChatGptSessionId, CodexReasoning,
    CodexTextVerbosity,
};

use super::identity::{PromptCacheKey, ProviderSessionId};

const CODEX_ENDPOINT: &str = "https://chatgpt.com/backend-api/codex";
const CODEX_REQUEST_TIMEOUT_SECONDS: u64 = 300;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CodexCacheMode {
    Off,
    OneHour,
    TwentyFourHours,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CodexEndpointIdentity {
    Production,
}

impl CodexEndpointIdentity {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Production => CODEX_ENDPOINT,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CodexResponsesSettingsDraft {
    model: String,
    endpoint: CodexEndpointIdentity,
    reasoning: Option<CodexReasoning>,
    text_verbosity: CodexTextVerbosity,
    cache_mode: CodexCacheMode,
}

impl CodexResponsesSettingsDraft {
    pub(crate) fn new(
        model: String,
        reasoning: Option<CodexReasoning>,
        cache_mode: CodexCacheMode,
    ) -> Self {
        Self {
            model,
            endpoint: CodexEndpointIdentity::Production,
            reasoning,
            text_verbosity: CodexTextVerbosity::Medium,
            cache_mode,
        }
    }

    pub(crate) fn finalize(self, provider_session_id: ProviderSessionId) -> CodexResponsesSettings {
        let prompt_cache_key = match self.cache_mode {
            CodexCacheMode::Off => None,
            CodexCacheMode::OneHour | CodexCacheMode::TwentyFourHours => {
                let key = PromptCacheKey::from_provider_session_id(&provider_session_id);
                Some(key.to_chatgpt_prompt_cache_key())
            }
        };
        CodexResponsesSettings {
            model: self.model,
            endpoint: self.endpoint,
            reasoning: self.reasoning,
            text_verbosity: self.text_verbosity,
            session_id: provider_session_id.to_chatgpt_session_id(),
            prompt_cache_key,
            store: false,
            request_timeout: Duration::from_secs(CODEX_REQUEST_TIMEOUT_SECONDS),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CodexResponsesSettings {
    model: String,
    endpoint: CodexEndpointIdentity,
    reasoning: Option<CodexReasoning>,
    text_verbosity: CodexTextVerbosity,
    session_id: ChatGptSessionId,
    prompt_cache_key: Option<ChatGptPromptCacheKey>,
    store: bool,
    request_timeout: Duration,
}

impl CodexResponsesSettings {
    pub(crate) fn model(&self) -> &str {
        &self.model
    }

    pub(crate) fn endpoint(&self) -> CodexEndpointIdentity {
        self.endpoint
    }

    pub(crate) fn store(&self) -> bool {
        self.store
    }

    pub(crate) fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    pub(crate) fn into_request_settings(self) -> ChatGptOAuthRequestSettings {
        ChatGptOAuthRequestSettings {
            reasoning: self.reasoning,
            text_verbosity: self.text_verbosity,
            session_id: self.session_id,
            prompt_cache_key: self.prompt_cache_key,
        }
    }
}

#[cfg(test)]
mod tests;
