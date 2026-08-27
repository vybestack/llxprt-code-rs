//! ChatGPT OAuth types.
//!
//! These are API request/response types for the ChatGPT OAuth flow.

#![allow(missing_docs)] // DTO fields are self-documenting

use crate::error::ModelError;
use serde::{Deserialize, Serialize};

const CODEX_LABEL_MAX_BYTES: usize = 64;

fn validate_codex_label(value: String, kind: &str) -> Result<String, ModelError> {
    if value.is_empty()
        || value.len() > CODEX_LABEL_MAX_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(ModelError::Configuration(format!(
            "{kind} must be 1-64 ASCII characters from [A-Za-z0-9_-]"
        )));
    }
    Ok(value)
}

/// Validated Codex session identity sent in the `session_id` header.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ChatGptSessionId(String);

impl ChatGptSessionId {
    pub fn new(value: impl Into<String>) -> Result<Self, ModelError> {
        validate_codex_label(value.into(), "ChatGPT session id").map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Validated Codex prompt-cache identity sent in the request body.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ChatGptPromptCacheKey(String);

impl ChatGptPromptCacheKey {
    pub fn new(value: impl Into<String>) -> Result<Self, ModelError> {
        validate_codex_label(value.into(), "ChatGPT prompt cache key").map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Codex reasoning effort supported by the Phase 0.5 request contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CodexReasoningEffort {
    High,
}

/// Codex reasoning summary mode supported by the Phase 0.5 request contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CodexReasoningSummary {
    Auto,
}

/// Codex text verbosity supported by the Phase 0.5 request contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CodexTextVerbosity {
    Medium,
}

/// Native Codex reasoning request settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CodexReasoning {
    pub effort: CodexReasoningEffort,
    pub summary: CodexReasoningSummary,
}

/// Typed settings consumed only by the ChatGPT OAuth request builder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatGptOAuthRequestSettings {
    pub reasoning: Option<CodexReasoning>,
    pub text_verbosity: CodexTextVerbosity,
    pub session_id: ChatGptSessionId,
    pub prompt_cache_key: Option<ChatGptPromptCacheKey>,
}

/// ChatGPT Codex API configuration.
#[derive(Clone)]
pub struct ChatGptConfig {
    /// Base URL for the Codex API.
    pub api_base_url: String,
    /// Model prefix for display.
    pub prefix: String,
    /// Default context length.
    pub context_length: usize,
}

impl std::fmt::Debug for ChatGptConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChatGptConfig")
            .field("api_base_url", &"[hidden]")
            .field("prefix", &self.prefix)
            .field("context_length", &self.context_length)
            .finish()
    }
}

impl Default for ChatGptConfig {
    fn default() -> Self {
        Self {
            api_base_url: "https://chatgpt.com/backend-api/codex".to_string(),
            prefix: "chatgpt-".to_string(),
            context_length: 272000,
        }
    }
}

/// Request body for ChatGPT Codex Responses API.
#[derive(Debug, Serialize)]
pub struct CodexRequest {
    pub model: String,
    pub instructions: String,
    pub input: Vec<InputItem>,
    pub store: bool,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<CodexReasoning>,
    pub text: CodexTextConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<ChatGptPromptCacheKey>,
}

#[derive(Debug, Serialize)]
pub struct CodexTextConfig {
    pub verbosity: CodexTextVerbosity,
}

/// Function call from assistant for Responses API.
#[derive(Debug, Serialize, Clone)]
pub struct FunctionCallItem {
    #[serde(rename = "type")]
    pub call_type: String,
    pub name: String,
    pub arguments: String,
    pub call_id: String,
}

/// Function call output for Responses API.
#[derive(Debug, Serialize, Clone)]
pub struct FunctionCallOutput {
    #[serde(rename = "type")]
    pub output_type: String,
    pub call_id: String,
    pub output: String,
}

/// Input item for Responses API.
#[derive(Debug, Serialize, Clone)]
#[serde(untagged)]
pub enum InputItem {
    Message(CodexMessage),
    FunctionCall(FunctionCallItem),
    FunctionOutput(FunctionCallOutput),
}

/// Message in a Codex request.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CodexMessage {
    pub role: String,
    pub content: MessageContent,
}

/// Message content (string or parts array).
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

/// Content part for multi-modal messages.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type")]
pub enum ContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: ImageUrl },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ImageUrl {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Tool call in a legacy Chat Completions response.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

/// Legacy Chat Completions response retained by the existing response parser.
#[derive(Debug, Deserialize)]
pub struct CodexResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<Choice>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
pub struct Choice {
    pub index: u32,
    pub message: ResponseMessage,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ResponseMessage {
    pub role: String,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Debug, Deserialize, Default)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_labels_accept_exact_bounds_and_allowed_ascii() {
        for value in ["a".to_string(), "A0_-".to_string(), "z".repeat(64)] {
            assert_eq!(ChatGptSessionId::new(&value).unwrap().as_str(), value);
            assert_eq!(ChatGptPromptCacheKey::new(&value).unwrap().as_str(), value);
        }
    }

    #[test]
    fn codex_labels_reject_invalid_values() {
        for value in ["", "a b", "é", "a/b", &"z".repeat(65)] {
            assert!(ChatGptSessionId::new(value).is_err(), "accepted {value:?}");
            assert!(
                ChatGptPromptCacheKey::new(value).is_err(),
                "accepted {value:?}"
            );
        }
    }

    #[test]
    fn typed_codex_settings_serialize_to_native_values() {
        let reasoning = CodexReasoning {
            effort: CodexReasoningEffort::High,
            summary: CodexReasoningSummary::Auto,
        };
        assert_eq!(
            serde_json::to_value(reasoning).unwrap(),
            serde_json::json!({"effort": "high", "summary": "auto"})
        );
        assert_eq!(
            serde_json::to_value(CodexTextConfig {
                verbosity: CodexTextVerbosity::Medium
            })
            .unwrap(),
            serde_json::json!({"verbosity": "medium"})
        );
    }

    #[test]
    fn default_endpoint_is_fixed_production_endpoint() {
        assert_eq!(
            ChatGptConfig::default().api_base_url,
            "https://chatgpt.com/backend-api/codex"
        );
    }
}
