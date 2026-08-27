//! ChatGPT OAuth model implementation.

use super::types::*;
use crate::error::ModelError;
use crate::model::{Model, ModelRequestParameters, StreamedResponse, ToolChoice};
use crate::profile::{openai_gpt4o_profile, ModelProfile};
use async_trait::async_trait;
use base64::Engine;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;
use serdes_ai_core::messages::{
    ImageContent, PartStartEvent, TextPart, ToolCallArgs, ToolCallPart, UserContent,
    UserContentPart, UserPromptPart,
};
use serdes_ai_core::{
    FinishReason, ModelRequest, ModelRequestPart, ModelResponse, ModelResponsePart, ModelSettings,
    RequestUsage,
};
use std::net::IpAddr;
use std::time::Duration;

pub(super) const CODEX_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
const ACCOUNT_ID_HEADER: HeaderName = HeaderName::from_static("chatgpt-account-id");
const ORIGINATOR_HEADER: HeaderName = HeaderName::from_static("originator");
const SESSION_ID_HEADER: HeaderName = HeaderName::from_static("session_id");

/// ChatGPT OAuth model.
#[derive(Clone)]
pub struct ChatGptOAuthModel {
    model_name: String,
    access_token: String,
    account_id: Option<String>,
    request_settings: Option<ChatGptOAuthRequestSettings>,
    client: Client,
    config: ChatGptConfig,
    profile: ModelProfile,
}

impl std::fmt::Debug for ChatGptOAuthModel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChatGptOAuthModel")
            .field("model_name", &self.model_name)
            .field("access_token", &"[redacted]")
            .finish_non_exhaustive()
    }
}

impl ChatGptOAuthModel {
    /// Creates a model fixed to the production ChatGPT Codex endpoint.
    pub fn new(model_name: impl Into<String>, access_token: impl Into<String>) -> Self {
        let model_name = model_name.into();
        let profile = Self::profile_for_model(&model_name);
        Self {
            model_name,
            access_token: access_token.into(),
            account_id: None,
            request_settings: None,
            client: crate::no_redirect_client(),
            config: ChatGptConfig::default(),
            profile,
        }
    }

    /// Applies a loopback-only configuration for local protocol fixtures.
    pub fn with_config(mut self, config: ChatGptConfig) -> Result<Self, ModelError> {
        let url = reqwest::Url::parse(&config.api_base_url)
            .map_err(|_| ModelError::configuration("ChatGPT loopback endpoint is invalid"))?;
        let host = url
            .host_str()
            .ok_or_else(|| ModelError::configuration("ChatGPT loopback endpoint has no host"))?;
        let is_loopback = host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback());
        if !is_loopback {
            return Err(ModelError::configuration(
                "ChatGPT endpoint overrides are limited to loopback hosts",
            ));
        }
        self.config = config;
        Ok(self)
    }

    /// Sets the ChatGPT account ID required by the Codex API.
    #[must_use]
    pub fn with_account_id(mut self, account_id: impl Into<String>) -> Self {
        self.account_id = Some(account_id.into());
        self
    }

    /// Sets the typed settings required by the Codex request builder.
    #[must_use]
    pub fn with_request_settings(mut self, settings: ChatGptOAuthRequestSettings) -> Self {
        self.request_settings = Some(settings);
        self
    }

    /// Sets a custom model profile.
    #[must_use]
    pub fn with_profile(mut self, profile: ModelProfile) -> Self {
        self.profile = profile;
        self
    }

    fn profile_for_model(model: &str) -> ModelProfile {
        let mut profile = openai_gpt4o_profile();
        if model.contains("o1") || model.contains("o3") {
            profile.supports_reasoning = true;
        }
        profile
    }

    fn account_id(&self) -> Result<&str, ModelError> {
        match self.account_id.as_deref() {
            Some(account_id) if !account_id.is_empty() => Ok(account_id),
            _ => Err(ModelError::configuration(
                "ChatGPT account id is required before request construction",
            )),
        }
    }

    fn request_settings(&self) -> Result<&ChatGptOAuthRequestSettings, ModelError> {
        self.request_settings.as_ref().ok_or_else(|| {
            ModelError::configuration(
                "ChatGPT OAuth request settings are required before request construction",
            )
        })
    }

    fn convert_user_content(&self, user: &UserPromptPart) -> MessageContent {
        match &user.content {
            UserContent::Text(text) => MessageContent::Text(text.clone()),
            UserContent::Parts(parts) => MessageContent::Parts(
                parts
                    .iter()
                    .filter_map(|part| match part {
                        UserContentPart::Text { text } => {
                            Some(ContentPart::Text { text: text.clone() })
                        }
                        UserContentPart::Image { image } => {
                            let url = match image {
                                ImageContent::Url(image) => image.url.clone(),
                                ImageContent::Binary(image) => format!(
                                    "data:{};base64,{}",
                                    image.media_type.mime_type(),
                                    base64::engine::general_purpose::STANDARD.encode(&image.data)
                                ),
                            };
                            Some(ContentPart::ImageUrl {
                                image_url: ImageUrl { url, detail: None },
                            })
                        }
                        _ => None,
                    })
                    .collect(),
            ),
        }
    }

    fn convert_tools(tools: &[serdes_ai_tools::ToolDefinition]) -> Vec<serde_json::Value> {
        tools
            .iter()
            .map(|tool| {
                serde_json::json!({
                    "type": "function",
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.parameters_json_schema
                })
            })
            .collect()
    }

    fn push_model_response(
        response: &ModelResponse,
        input: &mut Vec<InputItem>,
    ) -> Result<(), ModelError> {
        for part in &response.parts {
            match part {
                ModelResponsePart::Text(text) => input.push(InputItem::Message(CodexMessage {
                    role: "assistant".to_string(),
                    content: MessageContent::Text(text.content.clone()),
                })),
                ModelResponsePart::ToolCall(call) => {
                    let call_id = call.tool_call_id.as_deref().ok_or_else(|| {
                        ModelError::configuration("Codex assistant tool call requires a call id")
                    })?;
                    if call_id.is_empty() {
                        return Err(ModelError::configuration(
                            "Codex assistant tool call id must not be empty",
                        ));
                    }
                    input.push(InputItem::FunctionCall(FunctionCallItem {
                        call_type: "function_call".to_string(),
                        name: call.tool_name.clone(),
                        arguments: call.args_as_json_str()?,
                        call_id: call_id.to_string(),
                    }));
                }
                ModelResponsePart::Thinking(_)
                | ModelResponsePart::File(_)
                | ModelResponsePart::BuiltinToolCall(_) => {}
            }
        }
        Ok(())
    }

    fn collect_history(
        &self,
        messages: &[ModelRequest],
    ) -> Result<(String, Vec<InputItem>), ModelError> {
        let mut instructions = Vec::new();
        let mut input = Vec::new();
        for request in messages {
            for part in &request.parts {
                match part {
                    ModelRequestPart::SystemPrompt(system) => {
                        instructions.push(system.content.as_str());
                    }
                    ModelRequestPart::UserPrompt(user) => {
                        input.push(InputItem::Message(CodexMessage {
                            role: "user".to_string(),
                            content: self.convert_user_content(user),
                        }));
                    }
                    ModelRequestPart::ToolReturn(tool_return) => {
                        let call_id = tool_return.tool_call_id.as_deref().ok_or_else(|| {
                            ModelError::configuration("Codex tool return requires a call id")
                        })?;
                        if call_id.is_empty() {
                            return Err(ModelError::configuration(
                                "Codex tool return call id must not be empty",
                            ));
                        }
                        input.push(InputItem::FunctionOutput(FunctionCallOutput {
                            output_type: "function_call_output".to_string(),
                            call_id: call_id.to_string(),
                            output: tool_return.content.to_string_content(),
                        }));
                    }
                    ModelRequestPart::ModelResponse(response) => {
                        Self::push_model_response(response, &mut input)?;
                    }
                    ModelRequestPart::RetryPrompt(retry) => {
                        input.push(InputItem::Message(CodexMessage {
                            role: "user".to_string(),
                            content: MessageContent::Text(retry.content.message().to_string()),
                        }));
                    }
                    ModelRequestPart::BuiltinToolReturn(_) => {}
                }
            }
        }
        Ok((instructions.join("\n\n"), input))
    }

    pub(super) fn build_request(
        &self,
        messages: &[ModelRequest],
        _settings: &ModelSettings,
        params: &ModelRequestParameters,
    ) -> Result<CodexRequest, ModelError> {
        self.account_id()?;
        let request_settings = self.request_settings()?;
        let (instructions, input) = self.collect_history(messages)?;
        let tools = (!params.tools.is_empty()).then(|| Self::convert_tools(&params.tools));
        let tool_choice = params.tool_choice.as_ref().map(|choice| match choice {
            ToolChoice::Auto => serde_json::json!("auto"),
            ToolChoice::Required => serde_json::json!("required"),
            ToolChoice::None => serde_json::json!("none"),
            ToolChoice::Specific(name) => {
                serde_json::json!({"type": "function", "name": name})
            }
        });
        Ok(CodexRequest {
            model: self.model_name.clone(),
            instructions,
            input,
            store: false,
            stream: true,
            tools,
            tool_choice,
            reasoning: request_settings.reasoning.clone(),
            text: CodexTextConfig {
                verbosity: request_settings.text_verbosity,
            },
            prompt_cache_key: request_settings.prompt_cache_key.clone(),
        })
    }

    fn build_headers(&self) -> Result<HeaderMap, ModelError> {
        let account_id = self.account_id()?;
        let settings = self.request_settings()?;
        let authorization = format!("Bearer {}", self.access_token);
        let mut authorization = HeaderValue::from_str(&authorization).map_err(|_| {
            ModelError::configuration("ChatGPT authorization cannot be encoded as an HTTP header")
        })?;
        authorization.set_sensitive(true);
        let account_id = HeaderValue::from_str(account_id).map_err(|_| {
            ModelError::configuration("ChatGPT account id cannot be encoded as an HTTP header")
        })?;
        let session_id = HeaderValue::from_str(settings.session_id.as_str()).map_err(|_| {
            ModelError::configuration("ChatGPT session id cannot be encoded as an HTTP header")
        })?;
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, authorization);
        headers.insert(ACCOUNT_ID_HEADER, account_id);
        headers.insert(ORIGINATOR_HEADER, HeaderValue::from_static("codex_cli_rs"));
        headers.insert(SESSION_ID_HEADER, session_id);
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        Ok(headers)
    }

    /// Convert a Chat Completions style response (kept for reference/fallback)
    #[allow(dead_code)]
    fn convert_response(&self, response: CodexResponse) -> ModelResponse {
        let mut parts = Vec::new();

        for choice in &response.choices {
            if let Some(content) = &choice.message.content {
                if !content.is_empty() {
                    parts.push(ModelResponsePart::Text(TextPart::new(content)));
                }
            }

            if let Some(tool_calls) = &choice.message.tool_calls {
                for tc in tool_calls {
                    let args: serde_json::Value = serde_json::from_str(&tc.function.arguments)
                        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                    parts.push(ModelResponsePart::ToolCall(
                        ToolCallPart::new(&tc.function.name, ToolCallArgs::Json(args))
                            .with_tool_call_id(&tc.id),
                    ));
                }
            }
        }

        let finish_reason = response
            .choices
            .first()
            .and_then(|c| c.finish_reason.as_ref())
            .map(|r| {
                crate::map_terminal_reason(
                    r,
                    &[
                        ("stop", FinishReason::Stop),
                        ("length", FinishReason::Length),
                        ("tool_calls", FinishReason::ToolCall),
                    ],
                )
            });

        let usage = response.usage.map(|u| RequestUsage {
            request_tokens: Some(u.prompt_tokens as u64),
            response_tokens: Some(u.completion_tokens as u64),
            total_tokens: Some(u.total_tokens as u64),
            cache_creation_tokens: None,
            cache_read_tokens: None,
            details: None,
        });

        ModelResponse {
            parts,
            model_name: Some(response.model),
            timestamp: chrono::Utc::now(),
            finish_reason,
            usage,
            vendor_id: Some(response.id),
            vendor_details: None,
            kind: "response".to_string(),
        }
    }
}

#[async_trait]
impl Model for ChatGptOAuthModel {
    fn name(&self) -> &str {
        &self.model_name
    }

    fn system(&self) -> &str {
        "chatgpt-oauth"
    }

    async fn request(
        &self,
        messages: &[ModelRequest],
        settings: &ModelSettings,
        params: &ModelRequestParameters,
    ) -> Result<ModelResponse, ModelError> {
        let request = self.build_request(messages, settings, params)?;
        let headers = self.build_headers()?;
        let url = format!("{}/responses", self.config.api_base_url);
        let response = self
            .client
            .post(&url)
            .headers(headers)
            .timeout(CODEX_REQUEST_TIMEOUT)
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            crate::response::error_text(response).await?;
            return Err(crate::response::status_error(status, None));
        }

        super::sse::parse_response(response).await
    }

    async fn request_stream(
        &self,
        messages: &[ModelRequest],
        settings: &ModelSettings,
        params: &ModelRequestParameters,
    ) -> Result<StreamedResponse, ModelError> {
        // For now, fall back to non-streaming
        // TODO: Implement proper SSE streaming
        let response = self.request(messages, settings, params).await?;

        use serdes_ai_core::messages::ModelResponseStreamEvent;

        let events: Vec<Result<ModelResponseStreamEvent, ModelError>> = response
            .parts
            .into_iter()
            .enumerate()
            .map(|(idx, part)| {
                Ok(ModelResponseStreamEvent::PartStart(PartStartEvent::new(
                    idx, part,
                )))
            })
            .collect();

        Ok(Box::pin(futures::stream::iter(events)))
    }

    fn profile(&self) -> &ModelProfile {
        &self.profile
    }
}
