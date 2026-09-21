use std::sync::atomic::{AtomicUsize, Ordering};

use serdes_ai::core::ModelRequest;
use serdes_ai::models::Model as _;
use serdes_ai::ModelSettings;

use crate::adapter::{schema_for, ChatBackend, LlmResult};
use crate::model::SerdeAiParams;

/// Host adapter for the vendored Anthropic Messages model.
pub(crate) struct AnthropicBackend {
    model: serdes_ai::models::anthropic::AnthropicModel,
    model_settings: ModelSettings,
    secrets: Vec<String>,
    calls: AtomicUsize,
}

impl AnthropicBackend {
    pub(crate) fn new(
        model: serdes_ai::models::anthropic::AnthropicModel,
        model_settings: ModelSettings,
    ) -> Self {
        Self {
            model,
            model_settings,
            secrets: Vec::new(),
            calls: AtomicUsize::new(0),
        }
    }

    pub(crate) fn with_secrets(mut self, secrets: Vec<String>) -> Self {
        self.secrets = secrets;
        self
    }

    async fn request_async(
        &self,
        requests: &[ModelRequest],
        tools: &[crate::tools::ToolSpec],
    ) -> Result<LlmResult, String> {
        let params = SerdeAiParams {
            tools: std::sync::Arc::new(tools.iter().map(schema_for).collect()),
        };
        let response = self
            .model
            .request(
                requests,
                &self.model_settings,
                &params.to_model_request_parameters(),
            )
            .await
            .map_err(|error| {
                let error = crate::transport::context_length_400(&error).unwrap_or(error);
                match &error {
                    serdes_ai::models::ModelError::InvalidResponse(detail)
                    | serdes_ai::models::ModelError::Network(detail) => {
                        format!("{error}: {detail}")
                    }
                    _ => match crate::transport::TransportFailure::from_model_error(&error) {
                        Some(failure) => failure.diagnostic(),
                        None => error.to_string(),
                    },
                }
            })?;
        Ok(LlmResult::from_response(&response, &self.secrets))
    }
}

impl ChatBackend for AnthropicBackend {
    fn request<'a>(
        &'a self,
        requests: &'a [ModelRequest],
        tools: &'a [crate::tools::ToolSpec],
    ) -> crate::adapter::ModelFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.request_async(requests, tools).await
        })
    }

    fn request_calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}
