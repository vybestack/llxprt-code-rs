//! Model adapter isolating the `serdes-ai` crate behind a local interface.
//!
//! Only `OpenAIChatModel::request` (Chat Completions) is used. The
//! `stream-first-response-timeout-ms` profile value is mapped into the non-streaming
//! model timeout. `maxOutputTokens`, `temperature`, `top_p`, and `seed` are bound onto
//! the SerdesAI settings; unsupported `top_k` is rejected during profile resolution. Tool schemas are built with
//! `serdes_ai_tools::ObjectJsonSchema` and passed as the `tools` array on every request.
//!
//! The agent talks to the model through the [`ChatBackend`] trait so tests can drive the
//! whole turn loop against a mock with no network.

use crate::agent::transport::TransportFailure;
use crate::model::{ModelConfig, SerdeAiParams, SerdeAiSettings};
use crate::session::RoundRecord;
use serdes_ai::core::{
    messages::ToolCallArgs,
    messages::{FinishReason, ModelResponse, ModelResponsePart},
    ModelRequest,
};
use serdes_ai::models::openai::OpenAIChatModel;
use serdes_ai::models::Model as _;

/// A single tool call emitted by the model.
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args_json: String,
}

/// The model's reply for one round.
#[derive(Debug)]
pub struct LlmResult {
    pub text: String,
    pub calls: Vec<ToolCall>,
    pub finish_reason: Option<FinishReason>,
}

impl From<&ModelResponse> for LlmResult {
    fn from(resp: &ModelResponse) -> Self {
        let mut text = String::new();
        let mut calls = Vec::new();
        for part in &resp.parts {
            match part {
                ModelResponsePart::Text(t) => text.push_str(&t.content),
                ModelResponsePart::ToolCall(tc) => {
                    let args_json = match &tc.args {
                        ToolCallArgs::Json(v) => v.to_string(),
                        ToolCallArgs::String(s) => s.clone(),
                    };
                    calls.push(ToolCall {
                        id: tc.tool_call_id.clone().unwrap_or_default(),
                        name: tc.tool_name.clone(),
                        args_json,
                    });
                }
                _ => {}
            }
        }
        let finish_reason = resp.finish_reason.clone();
        LlmResult {
            text,
            calls,
            finish_reason,
        }
    }
}

/// The model-facing backend: one request, one round.
pub trait ChatBackend {
    /// Send the accumulated parts and map the reply. A network/transport error becomes
    /// `Err(String)`; the agent turns that into a terminal failure.
    fn request(
        &self,
        requests: &[ModelRequest],
        tools: &[crate::tools::ToolSpec],
    ) -> Result<LlmResult, String>;

    /// Cumulative number of [`Self::request`] calls. Defaults to `0`; a mock can
    /// override this so replay-vs-network is asserted offline without an HTTP server.
    fn request_calls(&self) -> usize {
        0
    }
}

/// Turn a [`crate::tools::ToolSpec`] into a serdes-ai tool definition.
pub fn schema_for(t: &crate::tools::ToolSpec) -> serdes_ai::tools::ToolDefinition {
    let mut schema = serde_json::json!({ "type": "object" });
    let mut props = serde_json::Map::new();
    let mut required = Vec::new();
    for (name, js, req) in &t.properties {
        props.insert(name.clone(), js.clone());
        if *req {
            required.push(name.clone());
        }
    }
    schema["properties"] = serde_json::Value::Object(props);
    schema["additionalProperties"] = serde_json::Value::Bool(false);
    if !required.is_empty() {
        schema["required"] = serde_json::Value::Array(
            required
                .into_iter()
                .map(serde_json::Value::String)
                .collect(),
        );
    }
    serdes_ai::tools::ToolDefinition {
        name: t.name.clone(),
        description: t.description.clone(),
        parameters_json_schema: schema,
        strict: None,
        outer_typed_dict_key: None,
    }
}

/// Wraps the SerdesAI OpenAI chat model.
pub struct ModelAdapter {
    inner: OpenAIChatModel,
    timeout: std::time::Duration,
    max_output_tokens: Option<u64>,
    model_params: Option<crate::profile::ModelParams>,
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

/// Adapter error that maps onto the CLI error surface.
pub struct ModelErrorAdapter {
    pub key: &'static str,
    pub message: String,
    pub code: crate::envelope::Code,
}

impl std::fmt::Display for ModelErrorAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let rendered = format!("{}: {}", self.key, self.message);
        f.write_str(&crate::redact::scrub_and_bound_diagnostic(&rendered))
    }
}

impl std::fmt::Debug for ModelErrorAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for ModelErrorAdapter {}
/// Build the adapter from a resolved config; the key is read here and stays on the adapter.
pub fn make_adapter(config: &ModelConfig) -> Result<ModelAdapter, ModelErrorAdapter> {
    let base_url = config.base_url.full().to_string();
    if base_url.trim().is_empty() || base_url == "<redacted>" {
        return Err(ModelErrorAdapter {
            key: "base-url",
            message: "base-url must not be empty".into(),
            code: crate::envelope::Code::Config,
        });
    }
    // The base URL and the resolved key both carry fixed caps, enforced before the
    // adapter is constructed. [`ModelConfig::from_profile`] rejects 4097 bytes with a
    // fixed path-free message; the inline auth-key scrub depends on the 4096 cap, so a
    // longer value can never leak into the transport. The key stays a
    // [`ModelConfig::secret_values`] member by construction.
    if base_url.len() > crate::redact::MAX_ENDPOINT_BYTES {
        return Err(ModelErrorAdapter {
            key: "base-url",
            message: crate::redact::ENDPOINT_CAP_MESSAGE.to_string(),
            code: crate::envelope::Code::Config,
        });
    }
    if config.api_key.len() > crate::redact::MAX_KEY_BYTES {
        return Err(ModelErrorAdapter {
            key: "auth-key",
            message: crate::redact::KEY_CAP_MESSAGE.to_string(),
            code: crate::envelope::Code::Config,
        });
    }
    let timeout = config
        .timeout
        .unwrap_or(std::time::Duration::from_millis(900_000));
    // The structural dsflash discriminator travels as per-model request settings;
    // Standard Chat keeps the default (the wire key stays absent).
    let mut model = openai_chat_model(&config.model, &config.api_key, &base_url, timeout);
    if let Some(spec) = config
        .model_params
        .as_ref()
        .and_then(|params| params.chat_template_kwargs.as_ref())
    {
        let wire_effort = spec.reasoning_effort.map(|effort| match effort {
            crate::profile::DsflashEffort::Minimal => {
                serdes_ai::models::openai::ChatTemplateReasoningEffort::Minimal
            }
            crate::profile::DsflashEffort::Low => {
                serdes_ai::models::openai::ChatTemplateReasoningEffort::Low
            }
            crate::profile::DsflashEffort::Medium => {
                serdes_ai::models::openai::ChatTemplateReasoningEffort::Medium
            }
            crate::profile::DsflashEffort::High => {
                serdes_ai::models::openai::ChatTemplateReasoningEffort::High
            }
            crate::profile::DsflashEffort::Xhigh => {
                serdes_ai::models::openai::ChatTemplateReasoningEffort::Xhigh
            }
            crate::profile::DsflashEffort::Max => {
                serdes_ai::models::openai::ChatTemplateReasoningEffort::Max
            }
        });
        model = model.with_request_settings(
            serdes_ai::models::openai::OpenAIChatModelRequestSettings {
                chat_template_kwargs: Some(serdes_ai::models::openai::ChatTemplateKwargs {
                    enable_thinking: spec.enable_thinking,
                    reasoning_effort: wire_effort,
                }),
            },
        );
    }
    Ok(ModelAdapter {
        inner: model,
        timeout,
        max_output_tokens: config.max_output_tokens,
        model_params: config.model_params.clone(),
        calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    })
}

/// The chat-completions route for a profile base URL: an already-ending `/chat/completions`
/// stays untouched (never doubled), a bare origin keeps the documented `/v1` route, and
/// every declared path prefix (`/v1`, `/api/paas/v4`, `/serverless/v1`, ...) appends the
/// single `/chat/completions` suffix to that prefix.
/// [`crate::model::validate_base_url`] rejected a path that already carries the API
/// suffix, so it never reaches here to be doubled.
pub fn chat_route(base: &str) -> String {
    let t = base.trim_end_matches('/');
    if t.ends_with("/chat/completions") {
        return t.to_string();
    }
    let authority_start = t.find("://").map(|i| i + 3).unwrap_or(0);
    if !t[authority_start..].contains('/') {
        return format!("{t}/v1/chat/completions");
    }
    format!("{t}/chat/completions")
}

/// The fixed cap (bytes) for a base URL. [`crate::model::parse_base_url`] applies
/// the same cap before the adapter is built, so a longer endpoint never reaches
/// [`make_adapter`].
pub fn max_endpoint_bytes() -> usize {
    crate::redact::MAX_ENDPOINT_BYTES
}

/// Construct the SerdesAI OpenAI chat model. The base URL ends in `/v1` and the
/// route joins `/chat/completions`; the stored redacted
/// `scheme://host:port` rendering is never sent as the request URL.
pub fn openai_chat_model(
    model: &str,
    api_key: &str,
    base_url: &str,
    timeout: std::time::Duration,
) -> OpenAIChatModel {
    let normalized = if base_url.ends_with('/') {
        base_url.to_string()
    } else {
        format!("{base_url}/")
    };
    let mut m = OpenAIChatModel::new(model, api_key).with_timeout(timeout);
    let route = chat_route(&normalized);
    m = m.with_base_url(route);
    m
}

impl ModelAdapter {
    /// Send a request and map the reply, translating errors into the `Err(String)` the
    /// loop converts to a failed result.
    pub async fn request_async(
        &self,
        requests: &[ModelRequest],
        tools: &[crate::tools::ToolSpec],
    ) -> Result<LlmResult, String> {
        let tool_defs = tools.iter().map(schema_for).collect::<Vec<_>>();
        let params = SerdeAiParams {
            tools: std::sync::Arc::new(tool_defs),
        };
        let settings = SerdeAiSettings {
            timeout: Some(self.timeout),
            max_tokens: self.max_output_tokens,
            model_params: self.model_params.as_ref(),
        }
        .into_model_settings();
        let resp = self
            .inner
            .request(requests, &settings, &params.to_model_request_parameters())
            .await
            .map_err(|e| match TransportFailure::from_model_error(&e) {
                Some(failure) => failure.diagnostic(),
                None => e.to_string(),
            })?;
        Ok(LlmResult::from(&resp))
    }
}

impl ChatBackend for ModelAdapter {
    fn request(
        &self,
        requests: &[ModelRequest],
        tools: &[crate::tools::ToolSpec],
    ) -> Result<LlmResult, String> {
        use std::sync::atomic::Ordering;
        self.calls.fetch_add(1, Ordering::SeqCst);
        let tools = tools.to_vec();
        let requests = requests.to_vec();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("runtime: {e}"))?;
        rt.block_on(self.request_async(&requests, &tools))
    }

    fn request_calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// A conservative byte estimate of the serialized tool schemas carried on every request
/// (name + description + the parameters JSON schema plus per-tool JSON framing), folded
/// into the complete-request preflight. The estimate is a serialization over-count
/// with saturated arithmetic, so an oversized schema total can never wrap.
pub fn estimate_tool_schema_bytes(tools: &[crate::tools::ToolSpec]) -> usize {
    tools.iter().fold(0usize, |n, t| {
        let params = serde_json::to_vec(&t.properties)
            .map(|v| v.len())
            .unwrap_or(0);
        n.saturating_add(
            t.name
                .len()
                .saturating_add(t.description.len())
                .saturating_add(params)
                .saturating_add(64),
        )
    })
}

/// Build the system-prompt request that starts every model call.
pub fn system_request(prompt: &str) -> ModelRequest {
    let mut req = ModelRequest::new();
    req.add_system_prompt(prompt.to_string());
    req
}

/// Build a user-prompt request.
pub fn user_request(prompt: &str) -> ModelRequest {
    let mut req = ModelRequest::new();
    req.add_user_prompt(prompt.to_string());
    req
}

/// Replay one persisted round as an assistant response with its tool calls (raw args and
/// call ids preserved), followed by a matching tool-return request per call.
pub fn persisted_round_requests(round: &RoundRecord) -> Vec<ModelRequest> {
    let mut out = Vec::new();
    let mut resp = ModelResponse::new();
    if !round.assistant.is_empty() {
        resp.parts
            .push(ModelResponsePart::Text(serdes_ai::core::TextPart::new(
                round.assistant.clone(),
            )));
    }
    for call in &round.calls {
        let args: serde_json::Value =
            serde_json::from_str(&call.args).unwrap_or(serde_json::json!({}));
        resp.parts.push(ModelResponsePart::ToolCall(
            serdes_ai::core::ToolCallPart::new(call.name.clone(), ToolCallArgs::Json(args))
                .with_tool_call_id(call.id.clone()),
        ));
    }
    let mut req = ModelRequest::new();
    req.parts
        .push(serdes_ai::core::ModelRequestPart::ModelResponse(Box::new(
            resp,
        )));
    out.push(req);
    for call in &round.calls {
        out.push(tool_return_request(
            &call.name,
            &call.id,
            call.ok,
            &call.result,
        ));
    }
    out
}

/// Wrap one tool result into a request carrying the matching `tool_call_id`.
pub fn tool_return_request(
    tool_name: &str,
    call_id: &str,
    ok: bool,
    content: &str,
) -> ModelRequest {
    let part = if ok {
        serdes_ai::core::ToolReturnPart::new(tool_name, content.to_string())
            .with_tool_call_id(call_id)
    } else {
        serdes_ai::core::ToolReturnPart::error(tool_name, content).with_tool_call_id(call_id)
    };
    let mut req = ModelRequest::new();
    req.parts
        .push(serdes_ai::core::ModelRequestPart::ToolReturn(part));
    req
}

/// Wrap the assistant's previous [`LlmResult`] back into a request part so the provider
/// re-emits it as a real `assistant` message with preserved call ids before tool returns.
pub fn assistant_request(result: &LlmResult) -> ModelRequest {
    let mut resp = ModelResponse::new();
    if !result.text.is_empty() {
        resp.parts
            .push(ModelResponsePart::Text(serdes_ai::core::TextPart::new(
                result.text.clone(),
            )));
    }
    for call in &result.calls {
        let args: serde_json::Value =
            serde_json::from_str(&call.args_json).unwrap_or(serde_json::json!({}));
        resp.parts.push(ModelResponsePart::ToolCall(
            serdes_ai::core::ToolCallPart::new(call.name.clone(), ToolCallArgs::Json(args))
                .with_tool_call_id(call.id.clone()),
        ));
    }
    let mut req = ModelRequest::new();
    req.parts
        .push(serdes_ai::core::ModelRequestPart::ModelResponse(Box::new(
            resp,
        )));
    req
}

#[cfg(test)]
mod tests {
    #[test]
    fn public_openai_model_debug_redacts_credentials_and_endpoint_values() {
        let marker = "adapter-debug-secret-marker";
        let model = super::openai_chat_model(
            "model",
            marker,
            &format!("http://127.0.0.1:9/v1?credential={marker}"),
            std::time::Duration::from_secs(1),
        );
        let rendered = format!("{model:?}");
        assert!(!rendered.contains(marker));
        assert!(rendered.contains("[redacted]"));
    }
}
