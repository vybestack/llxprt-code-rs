//! OpenAI Chat Completions model implementation.

use super::stream::OpenAIStreamParser;
use super::types::*;
use crate::error::{ModelError, TransportDetail};
use crate::model::{Model, ModelRequestParameters, StreamedResponse, ToolChoice};
use crate::profile::{openai_gpt4o_profile, ModelProfile};
use async_trait::async_trait;
use base64::Engine;
use reqwest::Client;
use serdes_ai_core::messages::{
    ImageContent, RetryPromptPart, SystemPromptPart, TextPart, ThinkingPart, ToolCallArgs,
    ToolCallPart, ToolReturnPart, UserContent, UserContentPart, UserPromptPart,
};
use serdes_ai_core::{
    FinishReason, ModelRequest, ModelRequestPart, ModelResponse, ModelResponsePart, ModelSettings,
    RequestUsage,
};
use serdes_ai_tools::ToolDefinition;
use std::time::Duration;

/// Build the chat-completions URL from a base URL. Only `chat/completions` is
/// normalized: a bare origin (with or without a trailing slash), `/v1` or `/v1/`
/// all map to `/v1/chat/completions`; a URL that already ends in exactly
/// `/chat/completions` is preserved verbatim. There is no arbitrary-path behavior: the
/// host rejects any other prefix before a request is made.
pub fn endpoint_url(base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    if is_chat_completion_url(trimmed) {
        return trimmed.to_string();
    }
    if trimmed.ends_with("/v1") {
        return format!("{trimmed}/chat/completions");
    }
    format!("{trimmed}/v1/chat/completions")
}

/// Whether a trimmed base already names the chat-completions route.
fn is_chat_completion_url(trimmed: &str) -> bool {
    trimmed.ends_with("/chat/completions")
}

/// The public wrapper the host crate calls. Kept here so the openai model needs no extra
/// wiring.
pub fn chat_url(base: &str) -> String {
    endpoint_url(base)
}

/// OpenAI Chat Completions model.
#[derive(Clone)]
pub struct OpenAIChatModel {
    model_name: String,
    client: Client,
    api_key: String,
    base_url: String,
    organization: Option<String>,
    project: Option<String>,
    profile: ModelProfile,
    default_timeout: Duration,
    request_settings: OpenAIChatModelRequestSettings,
}

/// Per-model typed request-extension settings. Only construction decides whether
/// the dsflash `chat_template_kwargs` object reaches requests; the per-request
/// `ModelSettings` carries no extension.
#[derive(Debug, Clone, Default)]
pub struct OpenAIChatModelRequestSettings {
    /// The dsflash request extension; `None` keeps the wire key absent.
    pub chat_template_kwargs: Option<ChatTemplateKwargs>,
}

impl std::fmt::Debug for OpenAIChatModel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenAIChatModel")
            .field("model_name", &self.model_name)
            .field("api_key", &"[redacted]")
            .finish_non_exhaustive()
    }
}

impl OpenAIChatModel {
    /// Create a new OpenAI chat model.
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
            default_timeout: Duration::from_secs(120),
            request_settings: OpenAIChatModelRequestSettings::default(),
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

    /// Set the typed per-model request-extension settings.
    #[must_use]
    pub fn with_request_settings(mut self, settings: OpenAIChatModelRequestSettings) -> Self {
        self.request_settings = settings;
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
        if model.starts_with("o1") || model.starts_with("o3") {
            // o1/o3 reasoning models
            crate::profile::openai_o1_profile()
        } else {
            openai_gpt4o_profile()
        }
    }

    /// Convert our messages to OpenAI format.
    fn convert_messages(&self, requests: &[ModelRequest]) -> Vec<ChatMessage> {
        requests
            .iter()
            .flat_map(|req| self.convert_request(req))
            .collect()
    }

    /// Convert a single request to OpenAI messages.
    fn convert_request(&self, req: &ModelRequest) -> Vec<ChatMessage> {
        let mut messages = Vec::new();

        for part in &req.parts {
            match part {
                ModelRequestPart::SystemPrompt(sys) => {
                    messages.push(self.convert_system_prompt(sys));
                }
                ModelRequestPart::UserPrompt(user) => {
                    messages.push(self.convert_user_prompt(user));
                }
                ModelRequestPart::ToolReturn(tool_ret) => {
                    messages.push(self.convert_tool_return(tool_ret));
                }
                ModelRequestPart::RetryPrompt(retry) => {
                    messages.push(self.convert_retry_prompt(retry));
                }
                ModelRequestPart::BuiltinToolReturn(builtin) => {
                    // Convert builtin tool return to a tool message
                    let content_str = serde_json::to_string(&builtin.content)
                        .unwrap_or_else(|_| builtin.content_type().to_string());
                    messages.push(ChatMessage::tool(content_str, builtin.tool_call_id.clone()));
                }
                ModelRequestPart::ModelResponse(response) => {
                    // Add the assistant response to messages for proper alternation
                    self.add_response_to_messages(&mut messages, response);
                }
            }
        }

        messages
    }

    fn convert_system_prompt(&self, sys: &SystemPromptPart) -> ChatMessage {
        ChatMessage {
            role: "system".to_string(),
            content: Some(MessageContent::Text(sys.content.clone())),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }
    }

    fn convert_user_prompt(&self, user: &UserPromptPart) -> ChatMessage {
        ChatMessage {
            role: "user".to_string(),
            content: Some(self.convert_user_content(&user.content)),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }
    }

    fn convert_user_content(&self, content: &UserContent) -> MessageContent {
        match content {
            UserContent::Text(text) => MessageContent::Text(text.clone()),
            UserContent::Parts(parts) => MessageContent::Parts(
                parts
                    .iter()
                    .filter_map(|p| self.convert_content_part(p))
                    .collect(),
            ),
        }
    }

    fn convert_content_part(&self, part: &UserContentPart) -> Option<ContentPart> {
        match part {
            UserContentPart::Text { text } => Some(ContentPart::text(text)),
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
                Some(ContentPart::image_url(url))
            }
            // Skip unsupported content types for now
            _ => None,
        }
    }

    fn convert_tool_return(&self, tool_ret: &ToolReturnPart) -> ChatMessage {
        let content = tool_ret.content.to_string_content();
        ChatMessage {
            role: "tool".to_string(),
            content: Some(MessageContent::Text(content)),
            name: None,
            tool_calls: None,
            tool_call_id: tool_ret.tool_call_id.clone(),
        }
    }

    fn convert_retry_prompt(&self, retry: &RetryPromptPart) -> ChatMessage {
        ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text(retry.content.message().to_string())),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }
    }

    /// Convert a ModelResponse to an assistant ChatMessage.
    pub fn convert_response_to_message(&self, resp: &ModelResponse) -> ChatMessage {
        let mut content_parts = Vec::new();
        let mut tool_calls = Vec::new();

        for part in &resp.parts {
            match part {
                ModelResponsePart::Text(text) => {
                    content_parts.push(text.content.clone());
                }
                ModelResponsePart::ToolCall(tc) => {
                    tool_calls.push(ToolCall {
                        id: tc.tool_call_id.clone().unwrap_or_default(),
                        tool_type: "function".to_string(),
                        function: FunctionCall {
                            name: tc.tool_name.clone(),
                            arguments: tc.args.to_json_string().unwrap_or_default(),
                        },
                    });
                }
                ModelResponsePart::Thinking(_) => {
                    // Thinking parts not sent back to OpenAI
                }
                ModelResponsePart::File(_) => {
                    // File parts not sent back to OpenAI
                }
                ModelResponsePart::BuiltinToolCall(_) => {
                    // Builtin tool calls not sent back to OpenAI
                }
            }
        }

        // Always provide content as a string (empty if no text parts).
        // Some providers (e.g., Cerebras) break when content is null with tool_calls.
        let content = Some(MessageContent::Text(content_parts.join("")));

        ChatMessage {
            role: "assistant".to_string(),
            content,
            name: None,
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            tool_call_id: None,
        }
    }

    /// Add an assistant response to messages (for multi-turn conversations).
    pub fn add_response_to_messages(
        &self,
        messages: &mut Vec<ChatMessage>,
        response: &ModelResponse,
    ) {
        messages.push(self.convert_response_to_message(response));
    }

    /// Convert tool definitions to OpenAI format.
    fn convert_tools(&self, tools: &[ToolDefinition]) -> Vec<ChatTool> {
        tools
            .iter()
            .map(|t| {
                let params = serde_json::to_value(&t.parameters_json_schema)
                    .unwrap_or(serde_json::json!({}));

                if t.strict.unwrap_or(false) {
                    ChatTool::function_strict(&t.name, &t.description, params)
                } else {
                    ChatTool::function(&t.name, &t.description, params)
                }
            })
            .collect()
    }

    /// Convert tool choice.
    fn convert_tool_choice(&self, choice: &ToolChoice) -> ToolChoiceValue {
        match choice {
            ToolChoice::Auto => ToolChoiceValue::auto(),
            ToolChoice::Required => ToolChoiceValue::required(),
            ToolChoice::None => ToolChoiceValue::none(),
            ToolChoice::Specific(name) => ToolChoiceValue::function(name),
        }
    }

    /// Build the request body.
    fn build_request(
        &self,
        messages: &[ModelRequest],
        settings: &ModelSettings,
        params: &ModelRequestParameters,
        stream: bool,
    ) -> ChatCompletionRequest {
        let messages = self.convert_messages(messages);

        let tools = if params.tools.is_empty() {
            None
        } else {
            Some(self.convert_tools(&params.tools))
        };

        let tool_choice = params
            .tool_choice
            .as_ref()
            .map(|c| self.convert_tool_choice(c));

        let response_format = params.output_schema.as_ref().map(|schema| {
            let schema_value = serde_json::to_value(schema).unwrap_or(serde_json::json!({}));
            ResponseFormat::json_schema("output", schema_value, true)
        });

        ChatCompletionRequest {
            model: self.model_name.clone(),
            messages,
            temperature: settings.temperature,
            top_p: settings.top_p,
            max_tokens: settings.max_tokens,
            max_completion_tokens: None,
            stop: settings.stop.clone(),
            presence_penalty: settings.presence_penalty,
            frequency_penalty: settings.frequency_penalty,
            seed: settings.seed,
            tools,
            tool_choice,
            parallel_tool_calls: settings.parallel_tool_calls,
            response_format,
            user: None,
            stream: if stream { Some(true) } else { None },
            stream_options: if stream {
                Some(StreamOptions {
                    include_usage: true,
                })
            } else {
                None
            },
            logprobs: None,
            top_logprobs: None,
            chat_template_kwargs: self.request_settings.chat_template_kwargs.clone(),
        }
    }

    /// Parse OpenAI response to our format.
    ///
    /// Patch 2: tool-call arguments pass through `ToolCallArgs::from`; malformed args remain
    /// exact raw strings instead of being silently normalized to `{}`. Valid JSON is a semantic
    /// value and may be normalized on serialization. Patch 1: unknown provider `finish_reason` strings are
    /// preserved verbatim in `FinishReason::Other` instead of being coerced to `Stop`.
    fn parse_response(&self, resp: ChatCompletionResponse) -> Result<ModelResponse, ModelError> {
        let choice = resp
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| ModelError::invalid_response("No choices in response"))?;

        if choice.message.role != "assistant" {
            return Err(ModelError::invalid_response(
                "OpenAI response message role is not assistant",
            ));
        }

        let mut parts = Vec::new();

        // Check for refusal
        if let Some(refusal) = choice.message.refusal {
            return Err(ModelError::ContentFiltered(refusal));
        }

        // Handle reasoning_content (chain-of-thought from models like GLM-4)
        if let Some(reasoning) = choice.message.reasoning_content {
            if !reasoning.is_empty() {
                parts.push(ModelResponsePart::Thinking(ThinkingPart::new(reasoning)));
            }
        }

        if let Some(content) = choice.message.content {
            if !content.is_empty() {
                parts.push(ModelResponsePart::Text(TextPart::new(content)));
            }
        }

        if let Some(tool_calls) = choice.message.tool_calls {
            for tc in tool_calls {
                if tc.tool_type != "function" {
                    return Err(ModelError::invalid_response(
                        "OpenAI response tool call type is not function",
                    ));
                }
                // Patch 2: `ToolCallArgs::from` yields `ToolCallArgs::Json` for
                // well-formed JSON and keeps a raw `String` for malformed arguments.
                let args = ToolCallArgs::from(tc.function.arguments.clone());

                parts.push(ModelResponsePart::ToolCall(
                    ToolCallPart::new(tc.function.name, args).with_tool_call_id(tc.id),
                ));
            }
        }

        // Patch 1: unknown finish reason strings are preserved verbatim (the llxprt-code-rs
        // host treats an unrecognized reason as an error rather than a clean stop).
        let finish_reason = choice.finish_reason.map(|r| match r.as_str() {
            "stop" => FinishReason::Stop,
            "length" => FinishReason::Length,
            "content_filter" => FinishReason::ContentFilter,
            "tool_calls" => FinishReason::ToolCall,
            raw => crate::map_terminal_reason(raw, &[]),
        });

        let usage = resp.usage.map(|u| RequestUsage {
            request_tokens: Some(u.prompt_tokens),
            response_tokens: Some(u.completion_tokens),
            total_tokens: Some(u.total_tokens),
            cache_creation_tokens: None,
            cache_read_tokens: u.prompt_tokens_details.and_then(|d| d.cached_tokens),
            details: None,
        });

        Ok(ModelResponse {
            parts,
            model_name: Some(resp.model),
            timestamp: chrono::Utc::now(),
            finish_reason,
            usage,
            vendor_id: Some(resp.id),
            vendor_details: None,
            kind: "response".to_string(),
        })
    }

    /// Handle API error response. The bounded detail carries the status, the bounded
    /// body prefix, and any `Retry-After`; the classification is derived from those facts
    /// rather than the status alone, so a quota hard stop never masquerades as a throttle.
    fn handle_error_response(&self, _status: u16, detail: TransportDetail) -> ModelError {
        crate::response::status_transport_error(detail)
    }

    /// The exact request route. The host adapter derives the full chat-completions URL via
    /// [`chat_url`] and stores it as the base URL, so [`endpoint_url`] returns it
    /// unchanged instead of appending the suffix a second time.
    fn route(&self) -> String {
        endpoint_url(&self.base_url)
    }
}

#[async_trait]
impl Model for OpenAIChatModel {
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
        let body = self.build_request(messages, settings, params, false);

        let timeout = settings.timeout.unwrap_or(self.default_timeout);

        // A credential-less (local OpenAI-compatible) endpoint resolves to the empty key;
        // an empty key sends no Authorization header rather than a bare `Bearer ` value.
        let mut request = self
            .client
            .post(self.route())
            .header("Content-Type", "application/json")
            .timeout(timeout);
        if !self.api_key.is_empty() {
            request = request.header("Authorization", format!("Bearer {}", self.api_key));
        }

        if let Some(ref org) = self.organization {
            request = request.header("OpenAI-Organization", org);
        }
        if let Some(ref project) = self.project {
            request = request.header("OpenAI-Project", project);
        }

        let response = request.json(&body).send().await?;

        let status = response.status().as_u16();
        if !response.status().is_success() {
            // The bounded body stays out of the public diagnostic: it is carried as a
            // typed, bounded prefix so the host can classify (429 throttle versus
            // quota exhaustion versus 5xx) and render on its own scrubbed path.
            let detail = crate::response::transport_detail(response).await?;
            return Err(self.handle_error_response(status, detail));
        }

        self.parse_response(crate::response::json::<ChatCompletionResponse>(response).await?)
    }

    async fn request_stream(
        &self,
        messages: &[ModelRequest],
        settings: &ModelSettings,
        params: &ModelRequestParameters,
    ) -> Result<StreamedResponse, ModelError> {
        let body = self.build_request(messages, settings, params, true);

        let timeout = settings.timeout.unwrap_or(self.default_timeout);

        // Same credential-less rule as `request`: no key, no Authorization header.
        let mut request = self
            .client
            .post(self.route())
            .header("Content-Type", "application/json")
            .timeout(timeout);
        if !self.api_key.is_empty() {
            request = request.header("Authorization", format!("Bearer {}", self.api_key));
        }

        if let Some(ref org) = self.organization {
            request = request.header("OpenAI-Organization", org);
        }
        if let Some(ref project) = self.project {
            request = request.header("OpenAI-Project", project);
        }

        let response = request.json(&body).send().await?;

        let status = response.status().as_u16();
        if !response.status().is_success() {
            // The bounded body stays out of the public diagnostic: it is carried as a
            // typed, bounded prefix so the host can classify (429 throttle versus
            // quota exhaustion versus 5xx) and render on its own scrubbed path.
            let detail = crate::response::transport_detail(response).await?;
            return Err(self.handle_error_response(status, detail));
        }

        let parser = OpenAIStreamParser::new(crate::response::stream(response));

        Ok(Box::pin(parser))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_openai_model_new() {
        let model = OpenAIChatModel::new("gpt-4o", "sk-test-key");
        assert_eq!(model.name(), "gpt-4o");
        assert_eq!(model.system(), "openai");
    }

    #[test]
    fn test_chat_template_kwargs_wire_shape() {
        // A dsflash-constructed model serializes exactly the bounded object.
        let model = OpenAIChatModel::new("deepseek", "key").with_request_settings(
            OpenAIChatModelRequestSettings {
                chat_template_kwargs: Some(ChatTemplateKwargs {
                    enable_thinking: true,
                    reasoning_effort: Some(ChatTemplateReasoningEffort::High),
                }),
            },
        );
        let request =
            model.build_request(&[], &ModelSettings::default(), &Default::default(), false);
        let wire = serde_json::to_value(&request).unwrap();
        assert_eq!(
            wire["chat_template_kwargs"],
            serde_json::json!({"enable_thinking": true, "reasoning_effort": "high"})
        );

        // An omitted effort omits the key entirely.
        let without_effort = OpenAIChatModel::new("deepseek", "key").with_request_settings(
            OpenAIChatModelRequestSettings {
                chat_template_kwargs: Some(ChatTemplateKwargs {
                    enable_thinking: false,
                    reasoning_effort: None,
                }),
            },
        );
        let request = without_effort.build_request(
            &[],
            &ModelSettings::default(),
            &Default::default(),
            false,
        );
        let wire = serde_json::to_value(&request).unwrap();
        assert_eq!(
            wire["chat_template_kwargs"],
            serde_json::json!({"enable_thinking": false})
        );
    }

    #[test]
    fn test_chat_template_kwargs_absent_without_request_settings() {
        // Every non-dsflash Chat target constructs without request settings; the
        // key must be absent from the wire body, not null.
        let model = OpenAIChatModel::new("gpt-4o", "key");
        assert!(model.request_settings.chat_template_kwargs.is_none());
        let request =
            model.build_request(&[], &ModelSettings::default(), &Default::default(), false);
        let wire = serde_json::to_value(&request).unwrap();
        assert!(wire.get("chat_template_kwargs").is_none());
    }

    #[test]
    fn test_openai_model_builder() {
        let model = OpenAIChatModel::new("gpt-4o", "sk-test-key")
            .with_base_url("https://custom.api.com/v1")
            .with_organization("org-123")
            .with_timeout(Duration::from_secs(60));

        assert_eq!(model.base_url, "https://custom.api.com/v1");
        assert_eq!(model.organization, Some("org-123".to_string()));
        assert_eq!(model.default_timeout, Duration::from_secs(60));
    }

    #[test]
    fn test_profile_selection() {
        let gpt4 = OpenAIChatModel::new("gpt-4o", "key");
        assert!(gpt4.profile().supports_native_structured_output);
        assert!(gpt4.profile().supports_images);

        let o1 = OpenAIChatModel::new("o1-preview", "key");
        assert!(o1.profile().supports_reasoning);
        assert!(!o1.profile().supports_system_messages);
    }

    #[test]
    fn test_convert_system_message() {
        let model = OpenAIChatModel::new("gpt-4o", "key");
        let sys = SystemPromptPart::new("You are helpful.");

        let msg = model.convert_system_prompt(&sys);
        assert_eq!(msg.role, "system");
        assert!(matches!(
            msg.content,
            Some(MessageContent::Text(ref t)) if t == "You are helpful."
        ));
    }

    #[test]
    fn test_convert_tools() {
        use serdes_ai_tools::ObjectJsonSchema;

        let model = OpenAIChatModel::new("gpt-4o", "key");
        let tools = vec![ToolDefinition::new("search", "Search the web")
            .with_parameters(ObjectJsonSchema::new())];

        let converted = model.convert_tools(&tools);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0].function.name, "search");
    }

    #[test]
    fn test_build_request() {
        let model = OpenAIChatModel::new("gpt-4o", "key");
        let mut req = ModelRequest::new();
        req.add_user_prompt("Hello");
        let messages = vec![req];
        let settings = ModelSettings::new().temperature(0.7);
        let params = ModelRequestParameters::new();

        let req = model.build_request(&messages, &settings, &params, false);

        assert_eq!(req.model, "gpt-4o");
        assert_eq!(req.temperature, Some(0.7));
        assert!(req.stream.is_none());
    }

    #[test]
    fn test_build_request_stream() {
        let model = OpenAIChatModel::new("gpt-4o", "key");
        let mut req = ModelRequest::new();
        req.add_user_prompt("Hello");
        let messages = vec![req];
        let settings = ModelSettings::new();
        let params = ModelRequestParameters::new();

        let req = model.build_request(&messages, &settings, &params, true);

        assert_eq!(req.stream, Some(true));
        assert!(req.stream_options.is_some());
    }

    #[test]
    fn test_endpoint_route_derivation() {
        // Patch 3: bare origin (with or without trailing slash) and /v1 both map to
        // /v1/chat/completions; an already-full route is preserved, never doubled.
        assert_eq!(
            chat_url("http://127.0.0.1:8080"),
            "http://127.0.0.1:8080/v1/chat/completions"
        );
        assert_eq!(
            chat_url("http://127.0.0.1:8080/"),
            "http://127.0.0.1:8080/v1/chat/completions"
        );
        assert_eq!(
            chat_url("https://api.example.com/v1"),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(
            chat_url("https://api.example.com/v1/"),
            "https://api.example.com/v1/chat/completions"
        );
        // The transport helper only derives the suffix. The host validates and rejects this
        // arbitrary prefix before calling the transport.
        assert_eq!(
            chat_url("https://gate.example.com/prefix/v1"),
            "https://gate.example.com/prefix/v1/chat/completions"
        );
        // An already-full route is preserved, never doubled.
        assert_eq!(
            chat_url("http://127.0.0.1:8080/chat/completions"),
            "http://127.0.0.1:8080/chat/completions"
        );
        assert_eq!(
            chat_url("http://127.0.0.1:8080/v1/chat/completions"),
            "http://127.0.0.1:8080/v1/chat/completions"
        );
    }

    #[test]
    fn test_route_helper_knows_full_route() {
        // The host adapter passes the derived full route through with_base_url; endpoint_url
        // recognizes it and never appends the suffix a second time.
        let model = OpenAIChatModel::new("gpt-4o", "key")
            .with_base_url("http://127.0.0.1:8080/chat/completions");
        assert_eq!(model.route(), "http://127.0.0.1:8080/chat/completions");
    }

    /// A credential-less (local OpenAI-compatible) endpoint carries an empty api_key;
    /// the transport then sends no authorization header rather than an empty bearer
    /// token that the local server would have to parse.
    #[tokio::test]
    async fn empty_key_sends_no_authorization_header() {
        use crate::ModelRequestParameters;
        use serdes_ai_core::messages::UserPromptPart;
        use serdes_ai_core::{ModelRequest, ModelRequestPart, ModelSettings};
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            // The matcher names only the headers that must be present; the assertion
            // after the request proves no authorization header traveled with it.
            .and(header("content-type", "application/json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"id":"1","object":"chat.completion","created":1,"model":"local",
                        "choices":[{"index":0,"message":{"role":"assistant","content":"ok"},
                        "finish_reason":"stop"}]}"#,
            ))
            .expect(1)
            .mount(&server)
            .await;

        let model = OpenAIChatModel::new("local-model", "")
            .with_base_url(format!("{}/chat/completions", server.uri()));
        model
            .request(
                &[ModelRequest::with_parts(vec![
                    ModelRequestPart::UserPrompt(UserPromptPart::new("hi")),
                ])],
                &ModelSettings::default(),
                &ModelRequestParameters::default(),
            )
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].headers.get("authorization").is_none());
    }
}
