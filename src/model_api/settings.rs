use serdes_ai_responses::types::ReasoningSettings;

const CODEX_RESPONSES_ENDPOINT: &str = "wss://chatgpt.com/backend-api/codex/responses";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CodexEndpointIdentity {
    Production,
}

impl CodexEndpointIdentity {
    pub(crate) fn websocket_url(self) -> &'static str {
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

#[cfg(test)]
mod tests;
