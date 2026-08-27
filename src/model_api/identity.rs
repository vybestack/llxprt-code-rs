use crate::session::SessionId;
use serdes_ai::models::chatgpt_oauth::{ChatGptPromptCacheKey, ChatGptSessionId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProviderSessionId(String);

impl ProviderSessionId {
    pub(crate) fn from_session_id(session_id: &SessionId) -> Result<Self, &'static str> {
        if !is_valid_codex_label(&session_id.id) {
            return Err("session id must be 1-64 ASCII characters from [A-Za-z0-9_-]");
        }
        Ok(Self(session_id.id.clone()))
    }

    pub(crate) fn to_chatgpt_session_id(&self) -> ChatGptSessionId {
        ChatGptSessionId::new(self.0.clone())
            .expect("validated host session id must be a valid ChatGPT session id")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PromptCacheKey(String);

impl PromptCacheKey {
    pub(crate) fn from_provider_session_id(session_id: &ProviderSessionId) -> Self {
        debug_assert!(is_valid_codex_label(&session_id.0));
        Self(session_id.0.clone())
    }

    pub(crate) fn to_chatgpt_prompt_cache_key(&self) -> ChatGptPromptCacheKey {
        ChatGptPromptCacheKey::new(self.0.clone())
            .expect("validated host cache key must be a valid ChatGPT prompt cache key")
    }
}

fn is_valid_codex_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

#[cfg(test)]
mod tests;
