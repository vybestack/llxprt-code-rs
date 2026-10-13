use std::sync::atomic::{AtomicUsize, Ordering};

use serdes_ai::core::ModelRequest;
use serdes_ai::models::Model as _;
use serdes_ai::ModelSettings;

use crate::adapter::{schema_for, BackendError, ChatBackend, LlmResult};
use crate::model::SerdeAiParams;

/// Host adapter for the vendored Anthropic Messages model.
pub(crate) struct AnthropicBackend {
    model: serdes_ai::models::anthropic::AnthropicModel,
    model_settings: ModelSettings,
    calls: AtomicUsize,
    secrets: Vec<String>,
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
            secrets: Vec::new(),
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
    ) -> Result<LlmResult, BackendError> {
        let params = SerdeAiParams {
            tools: std::sync::Arc::new(tools.iter().map(schema_for).collect()),
        };
        // Measure only the provider call: no tool execution or idle gap is included.
        let started = std::time::Instant::now();
        let response = self
            .model
            .request(
                requests,
                &self.model_settings,
                &params.to_model_request_parameters(),
            )
            .await
            .map_err(|error| {
                let mut error = BackendError::from_model(error);
                if let BackendError::Transport(failure) = &mut error {
                    failure.observation.provider_call_ms =
                        Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
                }
                error
            })?;
        Ok(LlmResult::from_response(&response, &self.secrets))
    }
}

impl ChatBackend for AnthropicBackend {
    fn provider_class(&self) -> crate::context_gateway::ProviderClass {
        crate::context_gateway::ProviderClass::StatelessAttested
    }

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

#[cfg(test)]
mod tests;
