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
    runtime: tokio::runtime::Runtime,
    calls: AtomicUsize,
}

impl AnthropicBackend {
    pub(crate) fn new(
        model: serdes_ai::models::anthropic::AnthropicModel,
        model_settings: ModelSettings,
    ) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("runtime: {error}"))?;
        Ok(Self {
            model,
            model_settings,
            runtime,
            calls: AtomicUsize::new(0),
        })
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
            .map_err(|error| match &error {
                serdes_ai::models::ModelError::InvalidResponse(detail)
                | serdes_ai::models::ModelError::Network(detail) => {
                    format!("{error}: {detail}")
                }
                _ => match crate::agent::transport::TransportFailure::from_model_error(&error) {
                    Some(failure) => failure.diagnostic(),
                    None => error.to_string(),
                },
            })?;
        Ok(LlmResult::from(&response))
    }
}

impl ChatBackend for AnthropicBackend {
    fn request(
        &self,
        requests: &[ModelRequest],
        tools: &[crate::tools::ToolSpec],
    ) -> Result<LlmResult, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.runtime.block_on(self.request_async(requests, tools))
    }

    fn request_calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}
