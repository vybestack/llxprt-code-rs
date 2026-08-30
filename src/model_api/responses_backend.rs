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

    /// Offset just past the CRLF CRLF header/body separator.
    fn find_body_start(request: &[u8]) -> Option<usize> {
        request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|offset| offset + 4)
    }

    /// The codex HTTP wire contract, pinned offline: `store: false`,
    /// `stream: true`, no `max_output_tokens` (even when the settings carry
    /// one), no `previous_response_id`, and full input replay on every turn.
    #[test]
    fn codex_http_wire_contract_streams_without_store_or_cap_and_replays_input() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().unwrap().port();
        let (bodies_tx, bodies_rx) = std::sync::mpsc::channel::<serde_json::Value>();
        let server = std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};
            for round in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept codex turn");
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = stream.read(&mut buf).expect("read codex request");
                    assert!(n > 0, "codex connection closed before the body arrived");
                    request.extend_from_slice(&buf[..n]);
                    if let Some(body_start) = find_body_start(&request) {
                        let headers =
                            String::from_utf8_lossy(&request[..body_start]).to_lowercase();
                        let length = headers
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .and_then(|value| value.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if request.len() >= body_start + length {
                            break;
                        }
                    }
                }
                let body_start = find_body_start(&request).expect("separator");
                let body: serde_json::Value = serde_json::from_slice(&request[body_start..])
                    .expect("codex body must be JSON");
                bodies_tx.send(body).expect("send captured body");

                let turn = if round == 0 { "one" } else { "two" };
                let response_id = format!("resp_loopback_{round}");
                let mut object = serdes_ai_responses::types::ResponseObject::in_progress(
                    response_id.clone(),
                    1,
                    "loopback-codex",
                    &serde_json::from_value(
                        serde_json::json!({"model": "loopback-codex", "input": []}),
                    )
                    .unwrap(),
                );
                object.status = serdes_ai_responses::types::ResponseStatus::Completed;
                object.usage = Some(serdes_ai_responses::types::ResponseUsage {
                    input_tokens: Some(11),
                    output_tokens: Some(7),
                    total_tokens: Some(18),
                });
                let created = serdes_ai_responses::types::StreamEvent::ResponseCreated {
                    sequence_number: 0,
                    response: object.clone(),
                };
                let message = serdes_ai_responses::types::OutputItem::Message {
                    id: format!("msg_{round}"),
                    role: "assistant".to_string(),
                    status: serdes_ai_responses::types::OutputItemStatus::Completed,
                    content: vec![serdes_ai_responses::types::OutputContent::OutputText {
                        text: format!("codex turn {turn}"),
                        annotations: Vec::new(),
                    }],
                };
                let item_added = serdes_ai_responses::types::StreamEvent::OutputItemAdded {
                    sequence_number: 1,
                    output_index: 0,
                    item: message,
                };
                let text_delta = serdes_ai_responses::types::StreamEvent::OutputTextDelta {
                    sequence_number: 2,
                    item_id: format!("msg_{round}"),
                    output_index: 0,
                    content_index: 0,
                    delta: format!("codex turn {turn}"),
                };
                let item_done = serdes_ai_responses::types::StreamEvent::OutputItemDone {
                    sequence_number: 3,
                    output_index: 0,
                    item: serdes_ai_responses::types::OutputItem::Message {
                        id: format!("msg_{round}"),
                        role: "assistant".to_string(),
                        status: serdes_ai_responses::types::OutputItemStatus::Completed,
                        content: Vec::new(),
                    },
                };
                let completed = serdes_ai_responses::types::StreamEvent::ResponseCompleted {
                    sequence_number: 4,
                    response: object,
                };
                let sse = |event: &serdes_ai_responses::types::StreamEvent| {
                    format!("data: {}\n\n", serde_json::to_string(event).unwrap())
                };
                // Round 0 ends like the real codex backend: the terminal
                // response event, then EOF, no `[DONE]` marker. Round 1
                // keeps the marker so both terminations stay covered.
                let done = if round == 0 {
                    String::new()
                } else {
                    "data: [DONE]\n\n".to_string()
                };
                let payload = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{}{}{}{}{}{}",
                    sse(&created),
                    sse(&item_added),
                    sse(&text_delta),
                    sse(&item_done),
                    sse(&completed),
                    done,
                );
                stream
                    .write_all(payload.as_bytes())
                    .expect("write SSE response");
            }
        });

        let model = OpenResponsesModel::new(
            "loopback-codex",
            format!("http://127.0.0.1:{port}/responses"),
        )
        .codex_http()
        .bearer("loopback-codex-key");
        let backend = ResponsesBackend::new(
            model,
            ModelSettings {
                // The cap is present in the settings; the codex mode must
                // still keep it off the wire.
                max_tokens: Some(40_000),
                timeout: Some(std::time::Duration::from_secs(10)),
                ..Default::default()
            },
        )
        .expect("test runtime must build");

        let turn = |text: &str| {
            ModelRequest::with_parts(vec![
                serdes_ai::core::messages::ModelRequestPart::UserPrompt(
                    serdes_ai::core::messages::UserPromptPart::new(text),
                ),
            ])
        };
        let first_history = vec![turn("first codex turn")];
        let second_history = vec![turn("first codex turn"), turn("second codex turn")];
        let first = backend
            .request(&first_history, &[])
            .expect("first codex turn");
        let second = backend
            .request(&second_history, &[])
            .expect("second codex turn");
        server.join().expect("server thread");

        let bodies: Vec<serde_json::Value> = bodies_rx.iter().collect();
        assert_eq!(bodies.len(), 2);
        for body in &bodies {
            assert_eq!(body["store"], false, "codex must never store");
            assert_eq!(body["stream"], true, "codex must stream over SSE");
            assert!(
                body.get("max_output_tokens").is_none(),
                "codex rejects max_output_tokens: {body}"
            );
            assert!(
                body.get("previous_response_id").is_none(),
                "nothing is stored, so nothing can chain"
            );
            assert!(body["input"].is_array(), "codex requires list input");
        }
        let replayed = serde_json::to_string(&bodies[1]["input"]).unwrap();
        assert!(
            replayed.contains("first codex turn") && replayed.contains("second codex turn"),
            "turn two must replay the full input, got: {replayed}"
        );
        assert!(
            first.text.contains("codex turn one"),
            "folded output missing: {first:?}"
        );
        assert!(
            second.text.contains("codex turn two"),
            "folded output missing: {second:?}"
        );
        assert_eq!(backend.request_calls(), 2);
    }

    /// A 200 whose body is JSON, not an SSE stream, must surface as an
    /// error rather than an empty turn: nothing was folded.
    #[test]
    fn codex_http_rejects_non_sse_success_body() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 4096];
            let mut request = Vec::new();
            loop {
                let n = stream.read(&mut buf).unwrap_or(0);
                assert!(n > 0, "connection closed before the body arrived");
                request.extend_from_slice(&buf[..n]);
                if let Some(body_start) = find_body_start(&request) {
                    let headers = String::from_utf8_lossy(&request[..body_start]).to_lowercase();
                    let length = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if request.len() >= body_start + length {
                        break;
                    }
                }
            }
            let body = br#"{"id":"r","object":"response","created_at":1,"status":"completed","model":"m","output":[]}"#;
            let payload = format!(
                "HTTP/1.1 200 OK
Content-Type: application/json
Content-Length: {}
Connection: close

",
                body.len()
            );
            stream.write_all(payload.as_bytes()).unwrap();
            stream.write_all(body).unwrap();
        });
        let model = OpenResponsesModel::new(
            "loopback-codex",
            format!("http://127.0.0.1:{port}/responses"),
        )
        .codex_http();
        let backend = ResponsesBackend::new(model, ModelSettings::default()).expect("runtime");
        let error = backend
            .request(&[ModelRequest::default()], &[])
            .expect_err("JSON body cannot fold into a codex turn");
        assert!(error.contains("sse stream"), "unexpected error: {error}");
        server.join().expect("server");
    }
}
