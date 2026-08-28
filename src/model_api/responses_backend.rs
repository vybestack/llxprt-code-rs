use std::sync::atomic::{AtomicUsize, Ordering};

use serdes_ai::core::ModelRequest;
use serdes_ai::models::Model as _;
use serdes_ai::ModelSettings;
use serdes_ai_responses::client::OpenResponsesModel;

use crate::adapter::{schema_for, ChatBackend, LlmResult};
use crate::model::SerdeAiParams;

pub(crate) struct ResponsesBackend {
    model: OpenResponsesModel,
    runtime: tokio::runtime::Runtime,
    calls: AtomicUsize,
}

impl ResponsesBackend {
    pub(crate) fn new(model: OpenResponsesModel) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("runtime: {error}"))?;
        Ok(Self {
            model,
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
                &ModelSettings::default(),
                &params.to_model_request_parameters(),
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok(LlmResult::from(&response))
    }
}

impl ChatBackend for ResponsesBackend {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_requests_are_counted_without_exposing_transport_details() {
        let backend = ResponsesBackend::new(OpenResponsesModel::new("test-model", "not-a-url"))
            .expect("test runtime must build");
        let error = backend
            .request(&[ModelRequest::default()], &[])
            .expect_err("invalid test endpoint must fail");

        assert_eq!(backend.request_calls(), 1);
        assert!(!error.contains("Bearer"));
        assert!(!error.contains("chatgpt-account-id"));
    }
}
