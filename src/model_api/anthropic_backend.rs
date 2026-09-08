use std::sync::atomic::{AtomicUsize, Ordering};

use serdes_ai::core::ModelRequest;
use serdes_ai::models::Model as _;
use serdes_ai::ModelSettings;

use crate::adapter::{schema_for, ChatBackend, LlmResult, ModelFailure};
use crate::model::SerdeAiParams;

/// Host adapter for the vendored Anthropic Messages model.
pub(crate) struct AnthropicBackend {
    model: serdes_ai::models::anthropic::AnthropicModel,
    model_settings: ModelSettings,
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
            calls: AtomicUsize::new(0),
        }
    }

    async fn request_async(
        &self,
        requests: &[ModelRequest],
        tools: &[crate::tools::ToolSpec],
    ) -> Result<LlmResult, ModelFailure> {
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
            .map_err(ModelFailure::from_model_error)?;
        Ok(LlmResult::from(&response))
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
