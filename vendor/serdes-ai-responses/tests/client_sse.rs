//! Owning transport regression: evidence escapes once; terminal errors never replay.
use futures::StreamExt;
use serdes_ai_core::messages::{ModelRequest, ModelResponseStreamEvent};
use serdes_ai_core::FinishReason;
use serdes_ai_core::ModelSettings;
use serdes_ai_models::model::{Model, ModelRequestParameters};
use serdes_ai_models::ModelError;
use serdes_ai_responses::client::OpenResponsesModel;
use serdes_ai_responses::types::{ErrorBodyRef, ResponseObject, ResponseStatus, StreamEvent};
use std::io::{Read, Write};

// These clients only talk to the in-process loopback fixture. Do not inherit
// CI's off-box proxy blackhole (or a developer's proxy) for that connection.
fn fixture_model(port: u16) -> OpenResponsesModel {
    OpenResponsesModel::new("fixture", format!("http://127.0.0.1:{port}/responses"))
        .with_http_client(reqwest::Client::builder().no_proxy().build().unwrap())
}

fn serve(body: Vec<u8>) -> (OpenResponsesModel, std::thread::JoinHandle<()>) {
    serve_http(body, true, 0)
}

fn serve_http(
    body: Vec<u8>,
    codex: bool,
    missing_bytes: usize,
) -> (OpenResponsesModel, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        loop {
            let mut buffer = [0; 4096];
            let n = socket.read(&mut buffer).unwrap();
            assert!(n > 0);
            request.extend_from_slice(&buffer[..n]);
            if let Some(start) = request.windows(4).position(|v| v == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..start]).to_lowercase();
                let length: usize = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .unwrap()
                    .parse()
                    .unwrap();
                if request.len() >= start + 4 + length {
                    break;
                }
            }
        }
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len() + missing_bytes).unwrap();
        // Keep byte fragmentation (including a split non-ASCII codepoint), but
        // do not dispatch the first frame until the entire semantic witness can
        // be queued. A rejected lifecycle frame legitimately closes the client
        // body; racing that closure with later-success writes tests the socket,
        // not whether the client rejects the contradiction before that success.
        let boundary = body.windows(2).position(|v| v == b"\n\n").unwrap_or(0);
        for chunk in body[..boundary].chunks(7) {
            socket.write_all(chunk).unwrap();
        }
        // One checked write queues the dispatch delimiter and all later frames.
        // A socket error or a short write is still a fixture failure, never an
        // excuse to discard a suffix required by the completion/error assertions.
        let tail = &body[boundary..];
        assert_eq!(socket.write(tail).unwrap(), tail.len());
        drop(socket);
        listener.set_nonblocking(true).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
    });
    let model = fixture_model(port);
    (if codex { model.codex_http() } else { model }, server)
}

#[tokio::test]
async fn partial_output_survives_terminal_provider_error_without_replay() {
    let body = concat!(
        "data: {\"type\":\"response.output_text.delta\",\"sequence_number\":1,\"item_id\":\"i\",\"output_index\":0,\"content_index\":0,\"delta\":\"evidence é\"}\n\n",
        "event: error\ndata: {\"type\":\"error\",\"code\":\"server_error\",\"message\":\"Bearer SECRET\"}\n\n"
    );
    let (model, server) = serve(body.as_bytes().to_vec());
    let mut stream = model
        .request_stream(
            &[ModelRequest::default()],
            &ModelSettings::default(),
            &ModelRequestParameters::default(),
        )
        .await
        .unwrap();
    let first = stream.next().await.unwrap().unwrap();
    assert!(format!("{first:?}").contains("evidence é"));
    let ModelError::Api {
        message: detail,
        code,
    } = stream.next().await.unwrap().unwrap_err()
    else {
        panic!("wrong terminal error")
    };
    assert!(detail.contains("server_error"));
    assert_eq!(code.as_deref(), Some("sse_provider_server_error"));
    assert!(!detail.contains("SECRET"));
    assert!(stream.next().await.is_none());
    server.join().unwrap();
}

#[tokio::test]
async fn saturated_event_channel_does_not_drop_terminal_error() {
    let mut body = String::new();
    for index in 0..80 {
        body.push_str(&format!("data: {{\"type\":\"response.output_text.delta\",\"sequence_number\":{index},\"item_id\":\"i\",\"output_index\":0,\"content_index\":0,\"delta\":\"x\"}}\n\n"));
    }
    body.push_str(&terminal(false));
    body.push_str("data: {\"type\":\"unknown\"}\n\n");
    let (model, server) = serve(body.into_bytes());
    let mut stream = model
        .request_stream(
            &[ModelRequest::default()],
            &ModelSettings::default(),
            &ModelRequestParameters::default(),
        )
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let mut count = 0;
    while let Some(item) = stream.next().await {
        match item {
            Ok(event) => {
                assert!(!matches!(
                    event,
                    ModelResponseStreamEvent::StreamComplete(_)
                ));
                count += 1;
            }
            Err(error) => {
                assert!(matches!(error, ModelError::InvalidResponse(_)));
                assert!(count >= 80);
                assert!(stream.next().await.is_none());
                server.join().unwrap();
                return;
            }
        }
    }
    panic!("terminal error lost after {count} events");
}

fn stalled_server() -> (OpenResponsesModel, std::thread::JoinHandle<()>) {
    stalled_server_with_prefix(String::new())
}

fn stalled_server_with_prefix(prefix: String) -> (OpenResponsesModel, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        let mut request = Vec::new();
        loop {
            let mut buffer = [0; 4096];
            let n = socket.read(&mut buffer).unwrap();
            assert!(n > 0);
            request.extend_from_slice(&buffer[..n]);
            if let Some(start) = request.windows(4).position(|v| v == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..start]).to_lowercase();
                let length: usize = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .unwrap()
                    .parse()
                    .unwrap();
                if request.len() >= start + 4 + length {
                    break;
                }
            }
        }
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").unwrap();
        if !prefix.is_empty() {
            write!(socket, "{:x}\r\n{}\r\n", prefix.len(), prefix).unwrap();
        }
        let mut byte = [0];
        match socket.read(&mut byte) {
            Ok(0) => {}
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
            other => panic!("cancelled owner left HTTP request alive: {other:?}"),
        }
        listener.set_nonblocking(true).unwrap();
        assert!(matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
    });
    (fixture_model(port).codex_http(), server)
}

#[tokio::test]
async fn dropping_stream_cancels_owning_http_body_without_replay() {
    let (model, server) = stalled_server();
    let stream = model
        .request_stream(
            &[ModelRequest::default()],
            &ModelSettings::default(),
            &ModelRequestParameters::default(),
        )
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    drop(stream);
    tokio::task::spawn_blocking(move || server.join().unwrap())
        .await
        .unwrap();
}

#[tokio::test]
async fn folded_request_deadline_cancels_owning_http_body_without_replay() {
    let (model, server) = stalled_server();
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        model.request(
            &[ModelRequest::default()],
            &ModelSettings::default(),
            &ModelRequestParameters::default(),
        ),
    )
    .await;
    assert!(result.is_err());
    tokio::task::spawn_blocking(move || server.join().unwrap())
        .await
        .unwrap();
}

#[tokio::test]
async fn interrupted_body_is_terminal_nonretryable_not_partial_success() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut buffer = [0; 4096];
        assert!(socket.read(&mut buffer).unwrap() > 0);
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 999\r\nConnection: close\r\n\r\ndata: {\"type\":\"keepalive\"}\n\n").unwrap();
    });
    let model = fixture_model(port).codex_http();
    let error = model
        .request(
            &[ModelRequest::default()],
            &ModelSettings::default(),
            &ModelRequestParameters::default(),
        )
        .await
        .unwrap_err();
    assert!(!error.is_retryable());
    let ModelError::InvalidResponse(detail) = error else {
        panic!("wrong error")
    };
    assert!(detail.contains("body read failed"));
    assert!(detail.contains("accumulated"));
    server.join().unwrap();
}

fn lifecycle_response(status: ResponseStatus, with_error: bool) -> ResponseObject {
    let request =
        serde_json::from_value(serde_json::json!({"model":"fixture","input":[]})).unwrap();
    let mut response = ResponseObject::in_progress("r", 1, "fixture", &request);
    response.status = status;
    if with_error {
        response.error = Some(ErrorBodyRef {
            code: "server_error".into(),
            message: "SECRET".into(),
        });
    }
    response
}

fn wire(event: StreamEvent) -> String {
    format!("data: {}\n\n", serde_json::to_string(&event).unwrap())
}

fn terminal(incomplete: bool) -> String {
    wire(if incomplete {
        StreamEvent::ResponseIncomplete {
            sequence_number: 2,
            response: lifecycle_response(ResponseStatus::Incomplete, false),
        }
    } else {
        StreamEvent::ResponseCompleted {
            sequence_number: 2,
            response: lifecycle_response(ResponseStatus::Completed, false),
        }
    })
}

fn progress() -> String {
    wire(StreamEvent::OutputTextDelta {
        sequence_number: 1,
        item_id: "i".into(),
        output_index: 0,
        content_index: 0,
        delta: "progress é".into(),
    })
}

async fn public_stream(model: &OpenResponsesModel) -> serdes_ai_models::StreamedResponse {
    model
        .request_stream(
            &[ModelRequest::default()],
            &ModelSettings::default(),
            &ModelRequestParameters::default(),
        )
        .await
        .unwrap()
}

async fn assert_progress_then_only_error(model: &OpenResponsesModel) -> ModelError {
    let mut stream = public_stream(model).await;
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        ModelResponseStreamEvent::PartDelta(_)
    ));
    let error = stream.next().await.unwrap().unwrap_err();
    assert!(!error.is_retryable());
    assert!(!format!("{error:?}").contains("SECRET"));
    assert!(stream.next().await.is_none());
    error
}

#[tokio::test]
async fn public_stream_completion_is_last_after_valid_eof_or_done() {
    for codex in [false, true] {
        for incomplete in [false, true] {
            for done in [false, true] {
                let suffix = if done { "data: [DONE]\n\n" } else { "" };
                let body = progress() + &terminal(incomplete) + suffix;
                let (model, server) = serve_http(body.into_bytes(), codex, 0);
                let mut stream = public_stream(&model).await;
                assert!(matches!(
                    stream.next().await.unwrap().unwrap(),
                    ModelResponseStreamEvent::PartDelta(_)
                ));
                let ModelResponseStreamEvent::StreamComplete(complete) =
                    stream.next().await.unwrap().unwrap()
                else {
                    panic!("missing validated completion")
                };
                assert_eq!(
                    complete.finish_reason,
                    if incomplete {
                        FinishReason::Length
                    } else {
                        FinishReason::Stop
                    }
                );
                assert!(stream.next().await.is_none());
                server.join().unwrap();
            }
        }
    }
}

#[tokio::test]
async fn public_stream_trailing_failures_never_publish_pending_completion() {
    for codex in [false, true] {
        for incomplete in [false, true] {
            for suffix in [
                "data: {\"type\":\"error\",\"code\":\"server_error\",\"message\":\"SECRET\"}\n\n"
                    .to_string(),
                "data: {\"type\":\"unknown\"}\n\n".to_string(),
                terminal(incomplete),
                "data: [DONE]\n\ndata: [DONE]\n\n".to_string(),
                "data: [DONE]\n\ndata: {\"type\":\"keepalive\"}\n\n".to_string(),
                "data: {".to_string(), // invalid EOF, even after a terminal
            ] {
                let (model, server) = serve_http(
                    (progress() + &terminal(incomplete) + &suffix).into_bytes(),
                    codex,
                    0,
                );
                assert_progress_then_only_error(&model).await;
                server.join().unwrap();
            }
        }
    }
}

#[tokio::test]
async fn public_stream_truncated_body_after_terminal_is_only_error() {
    for codex in [false, true] {
        let (model, server) = serve_http((progress() + &terminal(false)).into_bytes(), codex, 999);
        let error = assert_progress_then_only_error(&model).await;
        assert!(
            matches!(error, ModelError::InvalidResponse(ref detail) if detail.contains("body read failed"))
        );
        server.join().unwrap();
    }
}

#[tokio::test]
async fn public_stream_deadline_after_terminal_is_only_error() {
    let (model, server) = stalled_server_with_prefix(progress() + &terminal(false));
    let model = model.with_http_client(
        reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_millis(300))
            .build()
            .unwrap(),
    );
    assert_progress_then_only_error(&model).await;
    tokio::task::spawn_blocking(move || server.join().unwrap())
        .await
        .unwrap();
}

#[tokio::test]
async fn dropping_stream_with_pending_completion_cancels_validation_without_replay() {
    let (model, server) = stalled_server_with_prefix(progress() + &terminal(false));
    let mut stream = public_stream(&model).await;
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        ModelResponseStreamEvent::PartDelta(_)
    ));
    // Progress is delivered while the body is still open. A terminal must not be.
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), stream.next())
            .await
            .is_err()
    );
    drop(stream);
    tokio::task::spawn_blocking(move || server.join().unwrap())
        .await
        .unwrap();
}

#[tokio::test]
async fn lifecycle_contradictions_fail_folded_and_public_stream_before_later_success() {
    for created in [true, false] {
        for status in [
            ResponseStatus::Failed,
            ResponseStatus::Completed,
            ResponseStatus::Incomplete,
            ResponseStatus::InProgress,
        ] {
            let with_error = status == ResponseStatus::InProgress;
            let response = lifecycle_response(status, with_error);
            let event = if created {
                StreamEvent::ResponseCreated {
                    sequence_number: 0,
                    response,
                }
            } else {
                StreamEvent::ResponseInProgress {
                    sequence_number: 0,
                    response,
                }
            };
            let body = progress() + &wire(event) + &terminal(false);
            let (model, server) = serve(body.clone().into_bytes());
            let error = model
                .request(
                    &[ModelRequest::default()],
                    &ModelSettings::default(),
                    &ModelRequestParameters::default(),
                )
                .await
                .unwrap_err();
            assert!(
                matches!(error, ModelError::InvalidResponse(ref detail) if detail.contains("ambiguous") && detail.contains("accumulated 1"))
            );
            server.join().unwrap();
            for codex in [false, true] {
                let (model, server) = serve_http(body.clone().into_bytes(), codex, 0);
                let error = assert_progress_then_only_error(&model).await;
                assert!(
                    matches!(error, ModelError::InvalidResponse(ref detail) if detail.contains("ambiguous"))
                );
                server.join().unwrap();
            }
        }
    }
}

#[tokio::test]
async fn valid_nonterminal_lifecycle_remains_supported_in_folded_and_streaming_calls() {
    for status in [ResponseStatus::Queued, ResponseStatus::InProgress] {
        let mut response = lifecycle_response(status, false);
        response.metadata = Some(
            serde_json::from_value(serde_json::json!({
                "opaque": {"type": "error"}
            }))
            .unwrap(),
        );
        let mut body = wire(StreamEvent::ResponseCreated {
            sequence_number: 0,
            response,
        });
        body += &wire(StreamEvent::ResponseInProgress {
            sequence_number: 1,
            response: lifecycle_response(ResponseStatus::InProgress, false),
        });
        // A nested error in opaque keepalive payload remains transport metadata.
        body += "data: {\"type\":\"keepalive\",\"payload\":{\"type\":\"error\",\"message\":\"SECRET\"}}\n\n";
        body += &terminal(false);
        let (model, server) = serve(body.clone().into_bytes());
        assert!(model
            .request(
                &[ModelRequest::default()],
                &ModelSettings::default(),
                &ModelRequestParameters::default()
            )
            .await
            .is_ok());
        server.join().unwrap();
        let (model, server) = serve(body.into_bytes());
        let mut stream = public_stream(&model).await;
        assert!(matches!(
            stream.next().await.unwrap().unwrap(),
            ModelResponseStreamEvent::StreamComplete(_)
        ));
        assert!(stream.next().await.is_none());
        server.join().unwrap();
    }
}

mod tool_lifecycle;

#[tokio::test]
async fn tool_lifecycle_contradictions_fail_folded_and_public_sse_without_replay() {
    for case in ["item", "done", "terminal"] {
        let body: String = tool_lifecycle::events(Some(case))
            .into_iter()
            .map(wire)
            .collect();
        let (model, server) = serve(body.clone().into_bytes());
        let error = model
            .request(
                &[ModelRequest::default()],
                &ModelSettings::default(),
                &ModelRequestParameters::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, ModelError::InvalidResponse(_)));
        assert!(!error.is_retryable());
        server.join().unwrap();
        for codex in [false, true] {
            let (model, server) = serve_http(body.clone().into_bytes(), codex, 0);
            let mut stream = public_stream(&model).await;
            let mut errors = 0;
            while let Some(event) = stream.next().await {
                match event {
                    Ok(event) => assert!(!matches!(
                        event,
                        ModelResponseStreamEvent::StreamComplete(_)
                    )),
                    Err(error) => {
                        errors += 1;
                        assert!(matches!(error, ModelError::InvalidResponse(_)));
                        assert!(!error.is_retryable());
                        assert!(!error.to_string().contains("SECRET"));
                    }
                }
            }
            assert_eq!(errors, 1);
            server.join().unwrap();
        }
    }
}

#[tokio::test]
async fn consistent_tool_sse_preserves_raw_bytes_and_terminal_last() {
    let body: String = tool_lifecycle::events(None).into_iter().map(wire).collect();
    let (model, server) = serve(body.clone().into_bytes());
    let response = model
        .request(
            &[ModelRequest::default()],
            &ModelSettings::default(),
            &ModelRequestParameters::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        response
            .tool_call_parts()
            .next()
            .unwrap()
            .args
            .to_json_string()
            .unwrap(),
        "{} \n"
    );
    server.join().unwrap();
    for codex in [false, true] {
        let (model, server) = serve_http(body.clone().into_bytes(), codex, 0);
        let mut stream = public_stream(&model).await;
        let mut completed = false;
        let mut bytes = String::new();
        while let Some(event) = stream.next().await {
            assert!(!completed, "event after completion");
            match event.unwrap() {
                ModelResponseStreamEvent::PartDelta(delta) => {
                    if let serdes_ai_core::messages::ModelResponsePartDelta::ToolCall(delta) =
                        delta.delta
                    {
                        bytes.push_str(&delta.args_delta);
                    }
                }
                ModelResponseStreamEvent::StreamComplete(_) => completed = true,
                _ => {}
            }
        }
        assert!(completed);
        assert_eq!(bytes, "{} \n");
        server.join().unwrap();
    }
}
