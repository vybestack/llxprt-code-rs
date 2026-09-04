use serdes_ai_responses::types::ReasoningSettings;

const CODEX_RESPONSES_ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CodexEndpointIdentity {
    Production,
}

impl CodexEndpointIdentity {
    /// The scheme of this URL selects the vendored client's transport; the
    /// WebSocket transport stays available by switching the registration row
    /// and this constant back to `wss://`.
    pub(crate) fn responses_url(self) -> &'static str {
        match self {
            Self::Production => CODEX_RESPONSES_ENDPOINT,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CodexResponsesSettingsDraft {
    model: String,
    endpoint: CodexEndpointIdentity,
    reasoning_enabled: bool,
}

impl CodexResponsesSettingsDraft {
    pub(crate) fn new(model: String, reasoning_enabled: bool) -> Self {
        Self {
            model,
            endpoint: CodexEndpointIdentity::Production,
            reasoning_enabled,
        }
    }

    pub(crate) fn model(&self) -> &str {
        &self.model
    }

    pub(crate) fn endpoint(&self) -> CodexEndpointIdentity {
        self.endpoint
    }

    pub(crate) fn responses_reasoning(&self) -> Option<ReasoningSettings> {
        self.reasoning_enabled.then(|| ReasoningSettings {
            effort: Some("high".to_string()),
            summary: Some(serde_json::Value::String("auto".to_string())),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptCaching {
    Off,
    Cached,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AnthropicSettingsDraft {
    pub(crate) prompt_caching: PromptCaching,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OpenAiResponsesSettingsDraft {
    pub(crate) reasoning_effort: Option<serdes_ai::models::openai::ReasoningEffort>,
    pub(crate) reasoning_summary: Option<serdes_ai::models::openai::ReasoningSummary>,
    pub(crate) text_verbosity: Option<serdes_ai::models::openai::TextVerbosity>,
    pub(crate) prompt_caching: PromptCaching,
}

impl OpenAiResponsesSettingsDraft {
    pub(crate) fn finalize(
        &self,
        session_id: &crate::session::SessionId,
    ) -> serdes_ai::models::openai::OpenAIResponsesModelSettings {
        let cached = self.prompt_caching == PromptCaching::Cached;
        serdes_ai::models::openai::OpenAIResponsesModelSettings {
            reasoning_effort: self.reasoning_effort,
            reasoning_summary: self.reasoning_summary,
            send_reasoning_ids: false,
            previous_response_id: None,
            text_verbosity: self.text_verbosity,
            prompt_cache_key: cached.then(|| session_id.id.clone()),
            prompt_cache_retention: cached
                .then_some(serdes_ai::models::openai::PromptCacheRetention::Hours24),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests;
