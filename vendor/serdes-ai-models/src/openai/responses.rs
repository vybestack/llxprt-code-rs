//! OpenAI Responses API implementation.
//!
//! The Responses API is OpenAI's new API that supports native reasoning
//! models like o1, o3, and gpt-5 with built-in tool execution.
//!
//! Key differences from Chat Completions:
//! - Uses `input` instead of `messages`
//! - Has native reasoning support with configurable effort
//! - Supports built-in tools (web search, code interpreter, file search, etc.)
//! - Different output format with `ResponseOutputItem` variants

use crate::error::ModelError;
use crate::model::{Model, ModelRequestParameters, StreamedResponse};
use crate::profile::{openai_o1_profile, ModelProfile};
use async_trait::async_trait;
use base64::Engine;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use serdes_ai_core::messages::{
    ImageContent, TextPart, ThinkingPart, ToolCallArgs, ToolCallPart, UserContent, UserContentPart,
};
use serdes_ai_core::{
    FinishReason, ModelRequest, ModelRequestPart, ModelResponse, ModelResponsePart, ModelSettings,
    RequestUsage,
};
use serdes_ai_tools::ToolDefinition;
use std::time::Duration;

// ============================================================================
// Responses API Settings
// ============================================================================

/// OpenAI Responses API model settings.
#[derive(Debug, Clone, Default)]
pub struct OpenAIResponsesModelSettings {
    /// Reasoning effort: "low", "medium", "high"
    pub reasoning_effort: Option<ReasoningEffort>,

    /// Reasoning summary: "concise", "detailed", "auto"
    pub reasoning_summary: Option<ReasoningSummary>,

    /// Whether to send reasoning IDs back to the API (for continuation)
    pub send_reasoning_ids: bool,

    /// Include log probabilities
    pub logprobs: Option<bool>,

    /// Top logprobs count
    pub top_logprobs: Option<u32>,

    /// Service tier
    pub service_tier: Option<ServiceTier>,

    /// Truncation mode
    pub truncation: Option<TruncationMode>,

    /// Previous response ID for continuation
    pub previous_response_id: Option<String>,

    /// Text verbosity for generated output.
    pub text_verbosity: Option<TextVerbosity>,

    /// Stable prompt-cache identity for this stateless session.
    pub prompt_cache_key: Option<String>,

    /// Prompt-cache retention policy.
    pub prompt_cache_retention: Option<PromptCacheRetention>,
}

/// Text verbosity for generated output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TextVerbosity {
    /// Concise output.
    Low,
    /// Normal output detail.
    Medium,
    /// Detailed output.
    High,
}

/// Prompt-cache retention policy supported by the Responses API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PromptCacheRetention {
    /// Retain the cache entry for 24 hours.
    #[serde(rename = "24h")]
    Hours24,
}

/// Reasoning effort level for reasoning models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReasoningEffort {
    /// Minimal reasoning - fastest responses
    Low,
    /// Balanced reasoning - default
    #[default]
    Medium,
    /// Deep reasoning - most thorough
    High,
}

impl ReasoningEffort {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

impl Serialize for ReasoningEffort {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

/// Reasoning summary format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReasoningSummary {
    /// Concise summary of reasoning
    Concise,
    /// Detailed summary of reasoning
    Detailed,
    /// Let the model decide
    #[default]
    Auto,
}

impl ReasoningSummary {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Concise => "concise",
            Self::Detailed => "detailed",
            Self::Auto => "auto",
        }
    }
}

impl Serialize for ReasoningSummary {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

/// Service tier for API requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ServiceTier {
    /// Automatic tier selection
    #[default]
    Auto,
    /// Default tier
    Default,
    /// Flexible tier (may have variable latency)
    Flex,
    /// Priority tier (higher availability)
    Priority,
}

impl Serialize for ServiceTier {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let s = match self {
            Self::Auto => "auto",
            Self::Default => "default",
            Self::Flex => "flex",
            Self::Priority => "priority",
        };
        serializer.serialize_str(s)
    }
}

/// Truncation mode for input that exceeds context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TruncationMode {
    /// Disable truncation (error on overflow)
    #[default]
    Disabled,
    /// Auto-truncate older messages
    Auto,
}

impl Serialize for TruncationMode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let s = match self {
            Self::Disabled => "disabled",
            Self::Auto => "auto",
        };
        serializer.serialize_str(s)
    }
}

// ============================================================================
// Responses API Request Types
// ============================================================================

/// Request body for the Responses API.
#[derive(Debug, Clone, Serialize)]
pub struct ResponsesApiRequest {
    /// Model to use.
    pub model: String,
    /// Input messages/content.
    pub input: Vec<ResponseInput>,
    /// System instructions (replaces system message).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// Tool definitions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ResponseTool>>,
    /// Reasoning configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningConfig>,
    /// Maximum output tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    /// Sampling temperature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Top-p sampling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    /// Whether to stream the response.
    pub stream: bool,
    /// Previous response ID for multi-turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
    /// Service tier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<ServiceTier>,
    /// Truncation settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationConfig>,
    /// User identifier for tracking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Store response for later retrieval. Stateless clients always send `false`.
    pub store: bool,
    /// Text generation configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<ResponseTextConfig>,
    /// Stable prompt-cache identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
    /// Prompt-cache retention policy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_retention: Option<PromptCacheRetention>,
    /// Metadata for the request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<JsonValue>,
}

/// Text generation configuration.
#[derive(Debug, Clone, Serialize)]
pub struct ResponseTextConfig {
    /// Requested output verbosity.
    pub verbosity: TextVerbosity,
}

/// Reasoning configuration.
#[derive(Debug, Clone, Serialize)]
pub struct ReasoningConfig {
    /// Reasoning effort level.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<ReasoningEffort>,
    /// Summary format for reasoning.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<ReasoningSummary>,
}

/// Truncation configuration.
#[derive(Debug, Clone, Serialize)]
pub struct TruncationConfig {
    /// Truncation type.
    #[serde(rename = "type")]
    pub truncation_type: TruncationMode,
}

/// Input item for the Responses API.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
#[allow(missing_docs)]
pub enum ResponseInput {
    /// User or assistant message.
    Message {
        role: ResponseInputRole,
        content: ResponseInputContent,
    },
    /// A prior assistant function call.
    FunctionCall {
        #[serde(rename = "type")]
        item_type: FunctionCallInputType,
        call_id: String,
        name: String,
        arguments: String,
    },
    /// Output that matches a prior assistant function call.
    FunctionCallOutput {
        #[serde(rename = "type")]
        item_type: FunctionCallOutputInputType,
        call_id: String,
        output: String,
    },
}

/// Role for an input message.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ResponseInputRole {
    /// User-authored message.
    User,
    /// Assistant-authored message.
    Assistant,
}

/// Wire discriminator for a function-call input.
#[derive(Debug, Clone, Copy, Serialize)]
pub enum FunctionCallInputType {
    /// A function call made by the assistant.
    #[serde(rename = "function_call")]
    FunctionCall,
}

/// Wire discriminator for a function-call-output input.
#[derive(Debug, Clone, Copy, Serialize)]
pub enum FunctionCallOutputInputType {
    /// Output from a function call.
    #[serde(rename = "function_call_output")]
    FunctionCallOutput,
}

/// Content for response input.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum ResponseInputContent {
    /// Simple text.
    Text(String),
    /// Multi-part content.
    Parts(Vec<ResponseInputPart>),
}

/// Part of multi-part input content.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
#[allow(missing_docs)]
pub enum ResponseInputPart {
    /// Text content.
    #[serde(rename = "input_text")]
    Text { text: String },
    /// Image from URL.
    #[serde(rename = "input_image")]
    ImageUrl {
        image_url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Image from base64.
    #[serde(rename = "input_image")]
    ImageBase64 {
        image_url: String, // data:mime;base64,xxx format
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Audio input.
    #[serde(rename = "input_audio")]
    Audio { data: String, format: String },
}

// ============================================================================
// Responses API Tool Types
// ============================================================================

/// Tool definition for Responses API.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
#[allow(missing_docs)]
pub enum ResponseTool {
    /// Function tool (custom functions).
    #[serde(rename = "function")]
    Function {
        name: String,
        description: String,
        parameters: JsonValue,
        #[serde(skip_serializing_if = "Option::is_none")]
        strict: Option<bool>,
    },
    /// Web search built-in tool.
    #[serde(rename = "web_search_preview")]
    WebSearch {
        #[serde(skip_serializing_if = "Option::is_none")]
        search_context_size: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        user_location: Option<UserLocation>,
    },
    /// Code interpreter built-in tool.
    #[serde(rename = "code_interpreter")]
    CodeInterpreter {
        #[serde(skip_serializing_if = "Option::is_none")]
        container: Option<ContainerConfig>,
    },
    /// File search built-in tool.
    #[serde(rename = "file_search")]
    FileSearch {
        vector_store_ids: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        max_num_results: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        ranking_options: Option<RankingOptions>,
    },
    /// Image generation built-in tool.
    #[serde(rename = "image_generation")]
    ImageGeneration {
        #[serde(skip_serializing_if = "Option::is_none")]
        background: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        input_image_mask: Option<ImageMask>,
        #[serde(skip_serializing_if = "Option::is_none")]
        moderation: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        output_compression: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        output_format: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        partial_images: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        quality: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        size: Option<String>,
    },
    /// MCP (Model Context Protocol) tool.
    #[serde(rename = "mcp")]
    Mcp {
        server_label: String,
        server_url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        allowed_tools: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        headers: Option<JsonValue>,
    },
}

/// User location for web search.
#[derive(Debug, Clone, Serialize)]
#[allow(missing_docs)]
pub struct UserLocation {
    #[serde(rename = "type")]
    pub location_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
}

/// Container configuration for code interpreter.
#[derive(Debug, Clone, Serialize)]
#[allow(missing_docs)]
pub struct ContainerConfig {
    #[serde(rename = "type")]
    pub container_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_ids: Option<Vec<String>>,
}

/// Ranking options for file search.
#[derive(Debug, Clone, Serialize)]
#[allow(missing_docs)]
pub struct RankingOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ranker: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score_threshold: Option<f64>,
}

/// Image mask for image generation.
#[derive(Debug, Clone, Serialize)]
#[allow(missing_docs)]
pub struct ImageMask {
    pub image_url: String,
    #[serde(rename = "type")]
    pub mask_type: String,
}

// ============================================================================
// Responses API Response Types
// ============================================================================

/// Response from the Responses API.
#[derive(Debug, Clone, Deserialize)]
pub struct ResponsesApiResponse {
    /// Response ID.
    pub id: String,
    /// Object type (always "response").
    pub object: String,
    /// Creation timestamp.
    pub created_at: u64,
    /// Model used.
    pub model: String,
    /// Output items.
    pub output: Vec<ResponseOutputItem>,
    /// Token usage.
    pub usage: Option<ResponseUsage>,
    /// Response status.
    pub status: ResponseStatus,
    /// Error if any.
    pub error: Option<ResponseError>,
    /// Metadata.
    pub metadata: Option<JsonValue>,
    /// Service tier used.
    pub service_tier: Option<String>,
}

/// Response status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseStatus {
    /// Response is complete.
    Completed,
    /// Response failed.
    Failed,
    /// Response was cancelled.
    Cancelled,
    /// Response is incomplete (truncated).
    Incomplete,
    /// Response is in progress (streaming).
    InProgress,
}

/// Error in response.
#[derive(Debug, Clone, Deserialize)]
#[allow(missing_docs)]
pub struct ResponseError {
    pub code: String,
    pub message: String,
}

/// Output item from the Responses API.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
#[allow(missing_docs)]
pub enum ResponseOutputItem {
    /// Reasoning/thinking output.
    #[serde(rename = "reasoning")]
    Reasoning {
        id: String,
        #[serde(default)]
        summary: Vec<ReasoningSummaryItem>,
        status: Option<String>,
    },
    /// Text message output.
    #[serde(rename = "message")]
    Message {
        id: String,
        role: String,
        content: Vec<MessageContentItem>,
        status: Option<String>,
    },
    /// Function tool call.
    #[serde(rename = "function_call")]
    FunctionCall {
        id: String,
        call_id: String,
        name: String,
        arguments: String,
        status: Option<String>,
    },
    /// Function tool call output.
    #[serde(rename = "function_call_output")]
    FunctionCallOutput { call_id: String, output: String },
    /// Web search tool call.
    #[serde(rename = "web_search_call")]
    WebSearchCall { id: String, status: Option<String> },
    /// Code interpreter tool call.
    #[serde(rename = "code_interpreter_call")]
    CodeInterpreterCall {
        id: String,
        code: Option<String>,
        #[serde(default)]
        results: Vec<CodeInterpreterResult>,
        status: Option<String>,
    },
    /// File search tool call.
    #[serde(rename = "file_search_call")]
    FileSearchCall {
        id: String,
        #[serde(default)]
        results: Vec<FileSearchResult>,
        status: Option<String>,
    },
    /// Image generation tool call.
    #[serde(rename = "image_generation_call")]
    ImageGenerationCall {
        id: String,
        result: Option<ImageGenerationResult>,
        status: Option<String>,
    },
    /// MCP tool call.
    #[serde(rename = "mcp_call")]
    McpCall {
        id: String,
        server_label: String,
        tool_name: String,
        arguments: String,
        output: Option<String>,
        error: Option<String>,
        status: Option<String>,
    },
}

/// Reasoning summary item.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
#[allow(missing_docs)]
pub enum ReasoningSummaryItem {
    #[serde(rename = "summary_text")]
    Text { text: String },
}

/// Message content item.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
#[allow(missing_docs)]
pub enum MessageContentItem {
    #[serde(rename = "output_text")]
    Text {
        text: String,
        #[serde(default)]
        annotations: Vec<JsonValue>,
    },
    #[serde(rename = "refusal")]
    Refusal { refusal: String },
}

/// Code interpreter result.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
#[allow(missing_docs)]
pub enum CodeInterpreterResult {
    #[serde(rename = "logs")]
    Logs { logs: String },
    #[serde(rename = "image")]
    Image { image_url: String },
    #[serde(rename = "file")]
    File { file_id: String, filename: String },
}

/// File search result.
#[derive(Debug, Clone, Deserialize)]
#[allow(missing_docs)]
pub struct FileSearchResult {
    pub file_id: String,
    pub filename: String,
    pub score: Option<f64>,
    pub text: Option<String>,
}

/// Image generation result.
#[derive(Debug, Clone, Deserialize)]
#[allow(missing_docs)]
pub struct ImageGenerationResult {
    pub image: Option<String>, // base64 or URL
    pub revised_prompt: Option<String>,
}

/// Token usage for Responses API.
#[derive(Debug, Clone, Deserialize)]
#[allow(missing_docs)]
pub struct ResponseUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub input_tokens_details: Option<InputTokensDetails>,
    pub output_tokens_details: Option<OutputTokensDetails>,
}

/// Input token details.
#[derive(Debug, Clone, Deserialize)]
#[allow(missing_docs)]
pub struct InputTokensDetails {
    pub cached_tokens: Option<u64>,
}

/// Output token details.
#[derive(Debug, Clone, Deserialize)]
#[allow(missing_docs)]
pub struct OutputTokensDetails {
    pub reasoning_tokens: Option<u64>,
}

// ============================================================================
// OpenAI Responses Model
// ============================================================================

/// OpenAI Responses API model.
///
/// This model implements the new Responses API which supports native reasoning
/// models like o1, o3, and future gpt-5 variants.
///
/// # Example
///
/// ```rust,ignore
/// use serdes_ai_models::openai::{OpenAIResponsesModel, ReasoningEffort};
///
/// let model = OpenAIResponsesModel::from_env("o3-mini")?
///     .with_reasoning_effort(ReasoningEffort::High);
///
/// let response = model.request(&messages, &settings, &params).await?;
/// ```
#[derive(Clone)]
pub struct OpenAIResponsesModel {
    model_name: String,
    client: Client,
    api_key: String,
    base_url: String,
    organization: Option<String>,
    project: Option<String>,
    profile: ModelProfile,
    default_timeout: Duration,
    default_settings: OpenAIResponsesModelSettings,
}

impl std::fmt::Debug for OpenAIResponsesModel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenAIResponsesModel")
            .field("model_name", &self.model_name)
            .field("api_key", &"[redacted]")
            .finish_non_exhaustive()
    }
}

impl OpenAIResponsesModel {
    /// Create a new OpenAI Responses model.
    pub fn new(model_name: impl Into<String>, api_key: impl Into<String>) -> Self {
        let model_name = model_name.into();
        let profile = Self::profile_for_model(&model_name);

        Self {
            model_name,
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .build()
                .expect("the fixed no-redirect HTTP client configuration is valid"),
            api_key: api_key.into(),
            base_url: "https://api.openai.com/v1".to_string(),
            organization: None,
            project: None,
            profile,
            default_timeout: Duration::from_secs(300), // Longer for reasoning
            default_settings: OpenAIResponsesModelSettings::default(),
        }
    }

    /// Create from environment variable `OPENAI_API_KEY`.
    pub fn from_env(model_name: impl Into<String>) -> Result<Self, ModelError> {
        let api_key = std::env::var("OPENAI_API_KEY").map_err(|_| {
            ModelError::Configuration("OPENAI_API_KEY environment variable not set".to_string())
        })?;
        Ok(Self::new(model_name, api_key))
    }

    /// Set the base URL.
    #[must_use]
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Set default model-specific settings.
    #[must_use]
    pub fn with_settings(mut self, settings: OpenAIResponsesModelSettings) -> Self {
        self.default_settings = settings;
        self
    }

    /// Set the reasoning effort level.
    #[must_use]
    pub fn with_reasoning_effort(mut self, effort: ReasoningEffort) -> Self {
        self.default_settings.reasoning_effort = Some(effort);
        self
    }

    /// Set the reasoning summary format.
    #[must_use]
    pub fn with_reasoning_summary(mut self, summary: ReasoningSummary) -> Self {
        self.default_settings.reasoning_summary = Some(summary);
        self
    }

    /// Set the organization ID.
    #[must_use]
    pub fn with_organization(mut self, org: impl Into<String>) -> Self {
        self.organization = Some(org.into());
        self
    }

    /// Set the project ID.
    #[must_use]
    pub fn with_project(mut self, project: impl Into<String>) -> Self {
        self.project = Some(project.into());
        self
    }

    /// Set the default timeout.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }

    /// Set a custom profile.
    #[must_use]
    pub fn with_profile(mut self, profile: ModelProfile) -> Self {
        self.profile = profile;
        self
    }

    /// Get the appropriate profile for a model name.
    fn profile_for_model(model: &str) -> ModelProfile {
        // All Responses API models support reasoning
        if model.starts_with("o1") || model.starts_with("o3") || model.contains("gpt-5") {
            openai_o1_profile()
        } else {
            // Default to o1 profile for responses API
            openai_o1_profile()
        }
    }

    /// Convert the complete transcript to ordered Responses API input items.
    fn map_messages(
        &self,
        messages: &[ModelRequest],
    ) -> Result<(Vec<ResponseInput>, Option<String>), ModelError> {
        let mut inputs = Vec::new();
        let mut instructions = None;
        let mut prior_calls = std::collections::BTreeSet::new();
        let mut completed_calls = std::collections::BTreeSet::new();

        for req in messages {
            for part in &req.parts {
                match part {
                    ModelRequestPart::SystemPrompt(sys) => {
                        instructions = Some(sys.content.clone());
                    }
                    ModelRequestPart::UserPrompt(user) => inputs.push(ResponseInput::Message {
                        role: ResponseInputRole::User,
                        content: self.convert_user_content(&user.content)?,
                    }),
                    ModelRequestPart::ToolReturn(tool_ret) => {
                        let call_id = required_call_id(tool_ret.tool_call_id.as_deref())?;
                        if !prior_calls.contains(call_id) {
                            return Err(ModelError::api(
                                "tool output does not match a prior function call",
                            ));
                        }
                        if !completed_calls.insert(call_id.to_string()) {
                            return Err(ModelError::api("duplicate function call output"));
                        }
                        inputs.push(ResponseInput::FunctionCallOutput {
                            item_type: FunctionCallOutputInputType::FunctionCallOutput,
                            call_id: call_id.to_string(),
                            output: tool_ret.content.to_string_content(),
                        });
                    }
                    ModelRequestPart::RetryPrompt(retry) => inputs.push(ResponseInput::Message {
                        role: ResponseInputRole::User,
                        content: ResponseInputContent::Text(retry.content.message().to_string()),
                    }),
                    ModelRequestPart::BuiltinToolReturn(_) => {
                        return Err(ModelError::api(
                            "built-in tool output is unsupported by this Responses transport",
                        ));
                    }
                    ModelRequestPart::ModelResponse(response) => {
                        self.append_response_inputs(response, &mut inputs, &mut prior_calls)?;
                    }
                }
            }
        }

        Ok((inputs, instructions))
    }

    fn convert_user_content(
        &self,
        content: &UserContent,
    ) -> Result<ResponseInputContent, ModelError> {
        match content {
            UserContent::Text(text) => Ok(ResponseInputContent::Text(text.clone())),
            UserContent::Parts(parts) => {
                let converted = parts
                    .iter()
                    .map(|part| self.convert_content_part(part))
                    .collect::<Result<Vec<_>, _>>()?;
                if converted.len() == 1 {
                    if let ResponseInputPart::Text { text } = &converted[0] {
                        return Ok(ResponseInputContent::Text(text.clone()));
                    }
                }
                Ok(ResponseInputContent::Parts(converted))
            }
        }
    }

    fn convert_content_part(
        &self,
        part: &UserContentPart,
    ) -> Result<ResponseInputPart, ModelError> {
        match part {
            UserContentPart::Text { text } => Ok(ResponseInputPart::Text { text: text.clone() }),
            UserContentPart::Image { image } => {
                let url = match image {
                    ImageContent::Url(u) => u.url.clone(),
                    ImageContent::Binary(b) => {
                        format!(
                            "data:{};base64,{}",
                            b.media_type.mime_type(),
                            base64::engine::general_purpose::STANDARD.encode(&b.data)
                        )
                    }
                };
                Ok(ResponseInputPart::ImageUrl {
                    image_url: url,
                    detail: None,
                })
            }
            _ => Err(ModelError::api(
                "unsupported user content in Responses request",
            )),
        }
    }

    fn append_response_inputs(
        &self,
        response: &ModelResponse,
        inputs: &mut Vec<ResponseInput>,
        prior_calls: &mut std::collections::BTreeSet<String>,
    ) -> Result<(), ModelError> {
        for part in &response.parts {
            match part {
                ModelResponsePart::Text(text) => inputs.push(ResponseInput::Message {
                    role: ResponseInputRole::Assistant,
                    content: ResponseInputContent::Text(text.content.clone()),
                }),
                ModelResponsePart::ToolCall(call) => {
                    let call_id = required_call_id(call.tool_call_id.as_deref())?;
                    if !prior_calls.insert(call_id.to_string()) {
                        return Err(ModelError::api("duplicate function call identifier"));
                    }
                    inputs.push(ResponseInput::FunctionCall {
                        item_type: FunctionCallInputType::FunctionCall,
                        call_id: call_id.to_string(),
                        name: call.tool_name.clone(),
                        arguments: call.args.to_json_string()?,
                    });
                }
                ModelResponsePart::Thinking(_) => {}
                ModelResponsePart::File(_) | ModelResponsePart::BuiltinToolCall(_) => {
                    return Err(ModelError::api(
                        "unsupported assistant output in Responses transcript",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Convert tool definitions to Responses API format.
    fn convert_tools(&self, tools: &[ToolDefinition]) -> Vec<ResponseTool> {
        tools
            .iter()
            .map(|t| {
                let params = serde_json::to_value(&t.parameters_json_schema)
                    .unwrap_or(serde_json::json!({}));

                ResponseTool::Function {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: params,
                    strict: t.strict,
                }
            })
            .collect()
    }

    /// Build the request body.
    fn build_request(
        &self,
        messages: &[ModelRequest],
        settings: &ModelSettings,
        params: &ModelRequestParameters,
        stream: bool,
    ) -> Result<ResponsesApiRequest, ModelError> {
        let (input, instructions) = self.map_messages(messages)?;

        let tools = if params.tools.is_empty() {
            None
        } else {
            Some(self.convert_tools(&params.tools))
        };

        let reasoning = if self.default_settings.reasoning_effort.is_some()
            || self.default_settings.reasoning_summary.is_some()
        {
            Some(ReasoningConfig {
                effort: self.default_settings.reasoning_effort,
                summary: self.default_settings.reasoning_summary,
            })
        } else {
            None
        };

        let truncation = self
            .default_settings
            .truncation
            .map(|t| TruncationConfig { truncation_type: t });

        Ok(ResponsesApiRequest {
            model: self.model_name.clone(),
            input,
            instructions,
            tools,
            reasoning,
            max_output_tokens: settings.max_tokens,
            temperature: settings.temperature,
            top_p: settings.top_p,
            stream,
            previous_response_id: self.default_settings.previous_response_id.clone(),
            service_tier: self.default_settings.service_tier,
            truncation,
            user: None,
            store: false,
            text: self
                .default_settings
                .text_verbosity
                .map(|verbosity| ResponseTextConfig { verbosity }),
            prompt_cache_key: self.default_settings.prompt_cache_key.clone(),
            prompt_cache_retention: self.default_settings.prompt_cache_retention,
            metadata: None,
        })
    }

    /// Parse the Responses API response into our format.
    fn process_response(&self, resp: ResponsesApiResponse) -> Result<ModelResponse, ModelError> {
        if resp.object != "response" {
            return Err(ModelError::api(
                "provider returned an invalid response object",
            ));
        }
        if resp.status != ResponseStatus::Completed {
            return Err(ModelError::api(
                "provider returned a non-completed response",
            ));
        }
        if resp.error.is_some() {
            return Err(ModelError::api("provider returned a response error"));
        }

        let mut parts = Vec::new();
        let mut seen_call_ids = std::collections::BTreeSet::new();

        for output in resp.output {
            match output {
                ResponseOutputItem::Reasoning {
                    id,
                    summary,
                    status,
                } => {
                    require_completed_status(status.as_deref())?;
                    let content: String = summary
                        .iter()
                        .map(|s| match s {
                            ReasoningSummaryItem::Text { text } => text.as_str(),
                        })
                        .collect::<Vec<_>>()
                        .join("\n");

                    if !content.is_empty() {
                        let thinking = ThinkingPart::new(content)
                            .with_id(&id)
                            .with_provider_name("openai");
                        parts.push(ModelResponsePart::Thinking(thinking));
                    }
                }
                ResponseOutputItem::Message {
                    role,
                    content,
                    status,
                    ..
                } => {
                    if role != "assistant" {
                        return Err(ModelError::api("provider returned an invalid message role"));
                    }
                    require_completed_status(status.as_deref())?;
                    for item in content {
                        match item {
                            MessageContentItem::Text { text, .. } => {
                                if !text.is_empty() {
                                    parts.push(ModelResponsePart::Text(TextPart::new(text)));
                                }
                            }
                            MessageContentItem::Refusal { .. } => {
                                return Err(ModelError::ContentFiltered(
                                    "provider refused the response".to_string(),
                                ));
                            }
                        }
                    }
                }
                ResponseOutputItem::FunctionCall {
                    call_id,
                    name,
                    arguments,
                    status,
                    ..
                } => {
                    require_completed_status(status.as_deref())?;
                    let call_id = required_call_id(Some(&call_id))?;
                    if !seen_call_ids.insert(call_id.to_string()) {
                        return Err(ModelError::api("duplicate function call identifier"));
                    }
                    parts.push(ModelResponsePart::ToolCall(
                        ToolCallPart::new(name, ToolCallArgs::from(arguments))
                            .with_tool_call_id(call_id),
                    ));
                }
                ResponseOutputItem::WebSearchCall { .. }
                | ResponseOutputItem::CodeInterpreterCall { .. }
                | ResponseOutputItem::FileSearchCall { .. }
                | ResponseOutputItem::ImageGenerationCall { .. }
                | ResponseOutputItem::McpCall { .. }
                | ResponseOutputItem::FunctionCallOutput { .. } => {
                    return Err(ModelError::api(
                        "provider returned an unsupported output item",
                    ));
                }
            }
        }

        let finish_reason = if parts
            .iter()
            .any(|part| matches!(part, ModelResponsePart::ToolCall(_)))
        {
            Some(FinishReason::ToolCall)
        } else {
            Some(FinishReason::Stop)
        };

        let usage = resp.usage.map(|u| RequestUsage {
            request_tokens: Some(u.input_tokens),
            response_tokens: Some(u.output_tokens),
            total_tokens: Some(u.total_tokens),
            cache_creation_tokens: None,
            cache_read_tokens: u.input_tokens_details.and_then(|d| d.cached_tokens),
            details: u.output_tokens_details.map(|d| {
                let mut map = serde_json::Map::new();
                if let Some(reasoning) = d.reasoning_tokens {
                    map.insert("reasoning_tokens".to_string(), reasoning.into());
                }
                JsonValue::Object(map)
            }),
        });

        Ok(ModelResponse {
            parts,
            model_name: Some(resp.model),
            timestamp: chrono::Utc::now(),
            finish_reason,
            usage,
            vendor_id: Some(resp.id),
            vendor_details: resp.metadata,
            kind: "response".to_string(),
        })
    }

    /// Handle API error response.
    fn handle_error_response(&self, status: u16, _body: &str) -> ModelError {
        crate::response::status_error(status, None)
    }
}

fn required_call_id(call_id: Option<&str>) -> Result<&str, ModelError> {
    call_id
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ModelError::api("function call identifier is missing"))
}

fn require_completed_status(status: Option<&str>) -> Result<(), ModelError> {
    if status != Some("completed") {
        return Err(ModelError::api(
            "provider returned an invalid output status",
        ));
    }
    Ok(())
}

#[async_trait]
impl Model for OpenAIResponsesModel {
    fn name(&self) -> &str {
        &self.model_name
    }

    fn system(&self) -> &str {
        "openai"
    }

    fn profile(&self) -> &ModelProfile {
        &self.profile
    }

    async fn request(
        &self,
        messages: &[ModelRequest],
        settings: &ModelSettings,
        params: &ModelRequestParameters,
    ) -> Result<ModelResponse, ModelError> {
        let body = self.build_request(messages, settings, params, false)?;

        let timeout = settings.timeout.unwrap_or(self.default_timeout);

        let mut request = self
            .client
            .post(format!("{}/responses", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .timeout(timeout);

        if let Some(ref org) = self.organization {
            request = request.header("OpenAI-Organization", org);
        }
        if let Some(ref project) = self.project {
            request = request.header("OpenAI-Project", project);
        }

        let response = request.json(&body).send().await?;

        let status = response.status().as_u16();
        if !response.status().is_success() {
            let body = crate::response::error_text(response).await?;
            return Err(self.handle_error_response(status, &body));
        }

        let resp: ResponsesApiResponse = crate::response::json(response).await?;

        self.process_response(resp)
    }

    async fn request_stream(
        &self,
        messages: &[ModelRequest],
        settings: &ModelSettings,
        params: &ModelRequestParameters,
    ) -> Result<StreamedResponse, ModelError> {
        // For now, fall back to non-streaming
        // TODO: Implement proper streaming with ResponsesStreamParser
        let response = self.request(messages, settings, params).await?;

        // Convert to a single-event stream
        let events: Vec<Result<serdes_ai_core::messages::ModelResponseStreamEvent, ModelError>> =
            response
                .parts
                .into_iter()
                .enumerate()
                .map(|(idx, part)| {
                    Ok(
                        serdes_ai_core::messages::ModelResponseStreamEvent::PartStart(
                            serdes_ai_core::messages::PartStartEvent::new(idx, part),
                        ),
                    )
                })
                .collect();

        Ok(Box::pin(futures::stream::iter(events)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reasoning_effort_serialization() {
        assert_eq!(
            serde_json::to_string(&ReasoningEffort::Low).unwrap(),
            "\"low\""
        );
        assert_eq!(
            serde_json::to_string(&ReasoningEffort::Medium).unwrap(),
            "\"medium\""
        );
        assert_eq!(
            serde_json::to_string(&ReasoningEffort::High).unwrap(),
            "\"high\""
        );
    }

    #[test]
    fn test_reasoning_summary_serialization() {
        assert_eq!(
            serde_json::to_string(&ReasoningSummary::Concise).unwrap(),
            "\"concise\""
        );
        assert_eq!(
            serde_json::to_string(&ReasoningSummary::Detailed).unwrap(),
            "\"detailed\""
        );
    }

    #[test]
    fn test_model_creation() {
        let model = OpenAIResponsesModel::new("o3-mini", "sk-test");
        assert_eq!(model.name(), "o3-mini");
        assert_eq!(model.system(), "openai");
    }

    #[test]
    fn test_model_builder() {
        let model = OpenAIResponsesModel::new("o3-mini", "sk-test")
            .with_reasoning_effort(ReasoningEffort::High)
            .with_reasoning_summary(ReasoningSummary::Detailed)
            .with_base_url("https://custom.api.com/v1")
            .with_timeout(Duration::from_secs(600));

        assert_eq!(model.base_url, "https://custom.api.com/v1");
        assert_eq!(model.default_timeout, Duration::from_secs(600));
        assert_eq!(
            model.default_settings.reasoning_effort,
            Some(ReasoningEffort::High)
        );
    }

    #[test]
    fn test_response_tool_serialization() {
        let tool = ResponseTool::Function {
            name: "search".to_string(),
            description: "Search the web".to_string(),
            parameters: serde_json::json!({"type": "object"}),
            strict: Some(true),
        };
        let json = serde_json::to_string(&tool).unwrap();
        assert!(json.contains("\"type\":\"function\""));
        assert!(json.contains("\"name\":\"search\""));
    }

    #[test]
    fn test_web_search_tool_serialization() {
        let tool = ResponseTool::WebSearch {
            search_context_size: Some("medium".to_string()),
            user_location: None,
        };
        let json = serde_json::to_string(&tool).unwrap();
        assert!(json.contains("web_search_preview"));
    }

    #[test]
    fn test_response_input_serialization() {
        let input = ResponseInput::Message {
            role: ResponseInputRole::User,
            content: ResponseInputContent::Text("Hello".to_string()),
        };
        assert_eq!(
            serde_json::to_value(input).unwrap(),
            serde_json::json!({"role": "user", "content": "Hello"})
        );
    }

    #[test]
    fn test_tool_input_serialization() {
        let input = ResponseInput::FunctionCallOutput {
            item_type: FunctionCallOutputInputType::FunctionCallOutput,
            call_id: "call_123".to_string(),
            output: "Result: 42".to_string(),
        };
        assert_eq!(
            serde_json::to_value(input).unwrap(),
            serde_json::json!({
                "type": "function_call_output",
                "call_id": "call_123",
                "output": "Result: 42"
            })
        );
    }

    #[test]
    fn stateless_request_replays_ordered_function_history() {
        use serdes_ai_core::messages::{ToolReturnPart, UserPromptPart};

        let response = ModelResponse::with_parts(vec![
            ModelResponsePart::Text(TextPart::new("before")),
            ModelResponsePart::ToolCall(
                ToolCallPart::new("lookup", ToolCallArgs::String("{raw".to_string()))
                    .with_tool_call_id("call_1"),
            ),
            ModelResponsePart::Text(TextPart::new("after")),
        ]);
        let history = vec![ModelRequest::with_parts(vec![
            ModelRequestPart::UserPrompt(UserPromptPart::new("first")),
            ModelRequestPart::ModelResponse(Box::new(response)),
            ModelRequestPart::ToolReturn(
                ToolReturnPart::success("lookup", "result").with_tool_call_id("call_1"),
            ),
        ])];
        let model = OpenAIResponsesModel::new("o3-mini", "key").with_settings(
            OpenAIResponsesModelSettings {
                text_verbosity: Some(TextVerbosity::High),
                prompt_cache_key: Some("session_1".to_string()),
                prompt_cache_retention: Some(PromptCacheRetention::Hours24),
                ..Default::default()
            },
        );
        let request = model
            .build_request(
                &history,
                &ModelSettings::default(),
                &ModelRequestParameters::default(),
                false,
            )
            .unwrap();
        let value = serde_json::to_value(request).unwrap();
        assert_eq!(value["store"], false);
        assert!(value.get("previous_response_id").is_none());
        assert_eq!(value["text"]["verbosity"], "high");
        assert_eq!(value["prompt_cache_key"], "session_1");
        assert_eq!(value["prompt_cache_retention"], "24h");
        assert_eq!(
            value["input"],
            serde_json::json!([
                {"role":"user","content":"first"},
                {"role":"assistant","content":"before"},
                {"type":"function_call","call_id":"call_1","name":"lookup","arguments":"{raw"},
                {"role":"assistant","content":"after"},
                {"type":"function_call_output","call_id":"call_1","output":"result"}
            ])
        );
    }

    #[test]
    fn malformed_function_history_is_rejected_before_sending() {
        use serdes_ai_core::messages::ToolReturnPart;

        let unmatched = vec![ModelRequest::with_parts(vec![
            ModelRequestPart::ToolReturn(
                ToolReturnPart::success("lookup", "result").with_tool_call_id("unknown"),
            ),
        ])];
        let error = OpenAIResponsesModel::new("o3-mini", "key")
            .build_request(
                &unmatched,
                &ModelSettings::default(),
                &ModelRequestParameters::default(),
                false,
            )
            .unwrap_err();
        assert_eq!(error.to_string(), "Model API error");
    }

    #[test]
    fn malformed_function_arguments_remain_raw() {
        let response: ResponsesApiResponse = serde_json::from_value(serde_json::json!({
            "id": "resp_1",
            "object": "response",
            "created_at": 1,
            "model": "o3-mini",
            "output": [{
                "type": "function_call",
                "id": "item_1",
                "call_id": "call_1",
                "name": "search",
                "arguments": "<not-json>",
                "status": "completed"
            }],
            "usage": null,
            "status": "completed",
            "error": null,
            "metadata": null,
            "service_tier": null
        }))
        .unwrap();
        let result = OpenAIResponsesModel::new("o3-mini", "key")
            .process_response(response)
            .unwrap();
        assert!(matches!(
            &result.parts[0],
            ModelResponsePart::ToolCall(call)
                if call.args == ToolCallArgs::String("<not-json>".to_string())
        ));
        assert_eq!(result.finish_reason, Some(FinishReason::ToolCall));
    }

    #[test]
    fn response_object_status_role_and_item_status_are_strict() {
        let base = serde_json::json!({
            "id": "resp_1",
            "object": "response",
            "created_at": 1,
            "model": "o3-mini",
            "output": [{
                "type": "message",
                "id": "msg_1",
                "role": "assistant",
                "content": [{"type":"output_text","text":"done"}],
                "status": "completed"
            }],
            "status": "completed"
        });
        let invalid = [
            ("object", serde_json::json!("chat.completion")),
            ("status", serde_json::json!("incomplete")),
            ("output.0.role", serde_json::json!("user")),
            ("output.0.status", serde_json::Value::Null),
            ("output.0.status", serde_json::json!("in_progress")),
        ];
        for (path, replacement) in invalid {
            let mut value = base.clone();
            match path {
                "object" => value["object"] = replacement,
                "status" => value["status"] = replacement,
                "output.0.role" => value["output"][0]["role"] = replacement,
                "output.0.status" => value["output"][0]["status"] = replacement,
                _ => unreachable!(),
            }
            let response: ResponsesApiResponse = serde_json::from_value(value).unwrap();
            assert!(
                OpenAIResponsesModel::new("o3-mini", "key")
                    .process_response(response)
                    .is_err(),
                "{path}"
            );
        }
    }
}
