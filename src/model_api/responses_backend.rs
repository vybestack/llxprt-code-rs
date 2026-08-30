use std::sync::atomic::{AtomicUsize, Ordering};

use serdes_ai::core::ModelRequest;
use serdes_ai::models::Model as _;
use serdes_ai::ModelSettings;
use serdes_ai_responses::client::OpenResponsesModel;

use crate::adapter::{schema_for, ChatBackend, LlmResult};
use crate::model::SerdeAiParams;

enum ResponsesModel {
    Codex(OpenResponsesModel),
    OpenAi(Box<serdes_ai::models::openai::OpenAIResponsesModel>),
}

pub(crate) struct ResponsesBackend {
    model: ResponsesModel,
    model_settings: ModelSettings,
    runtime: tokio::runtime::Runtime,
    calls: AtomicUsize,
}

impl ResponsesBackend {
    pub(crate) fn new(
        model: OpenResponsesModel,
        model_settings: ModelSettings,
    ) -> Result<Self, String> {
        Self::with_model(ResponsesModel::Codex(model), model_settings)
    }

    pub(crate) fn new_openai(
        model: serdes_ai::models::openai::OpenAIResponsesModel,
        model_settings: ModelSettings,
    ) -> Result<Self, String> {
        Self::with_model(ResponsesModel::OpenAi(Box::new(model)), model_settings)
    }

    fn with_model(model: ResponsesModel, model_settings: ModelSettings) -> Result<Self, String> {
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
        let request_parameters = params.to_model_request_parameters();
        let response = match &self.model {
            ResponsesModel::Codex(model) => {
                model
                    .request(requests, &self.model_settings, &request_parameters)
                    .await
            }
            ResponsesModel::OpenAi(model) => {
                model
                    .request(requests, &self.model_settings, &request_parameters)
                    .await
            }
        }
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
        // The turn is bounded in the backend: neither the vendored Codex client nor
        // its WebSocket applies `ModelSettings::timeout`, so an unbounded `block_on`
        // here would let one request hang the agent forever.
        self.runtime.block_on(async {
            match self.model_settings.timeout {
                Some(limit) => tokio::time::timeout(limit, self.request_async(requests, tools))
                    .await
                    .map_err(|_| "responses request exceeded the configured timeout".to_string())
                    .and_then(|result| result),
                None => self.request_async(requests, tools).await,
            }
        })
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
        let backend = ResponsesBackend::new(
            OpenResponsesModel::new("test-model", "not-a-url"),
            ModelSettings::default(),
        )
        .expect("test runtime must build");
        let error = backend
            .request(&[ModelRequest::default()], &[])
            .expect_err("invalid test endpoint must fail");

        assert_eq!(backend.request_calls(), 1);
        assert!(!error.contains("Bearer"));
        assert!(!error.contains("chatgpt-account-id"));
    }

    #[test]
    fn codex_turn_is_bounded_by_the_configured_timeout() {
        use std::io::Read as _;

        // Accept the WebSocket TCP connection but never complete the handshake: the
        // client would wait forever, so only the backend bound can end the turn.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 512];
                let _ = stream.read(&mut buf);
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
        });

        let model = OpenResponsesModel::new("test-model", format!("ws://127.0.0.1:{port}"));
        let backend = ResponsesBackend::new(
            model,
            ModelSettings {
                timeout: Some(std::time::Duration::from_secs(1)),
                ..Default::default()
            },
        )
        .expect("test runtime must build");

        let started = std::time::Instant::now();
        let error = backend
            .request(&[ModelRequest::default()], &[])
            .expect_err("silent server must trip the backend timeout");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(4),
            "turn must end via the bound, not the socket"
        );
        assert_eq!(error, "responses request exceeded the configured timeout");
        assert_eq!(backend.request_calls(), 1);
    }
}
