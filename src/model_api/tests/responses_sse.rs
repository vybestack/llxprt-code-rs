use crate::adapter::ChatBackend;
use crate::model_api::responses_backend::tests::{
    codex_turn_sse_payload, read_codex_request, test_runtime,
};
use crate::model_api::responses_backend::ResponsesBackend;
use serdes_ai::core::{ModelRequest, ModelSettings};
use serdes_ai_responses::client::OpenResponsesModel;

fn loopback_sse(payload: String) -> (ResponsesBackend, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        use std::io::Write as _;
        let (mut stream, _) = listener.accept().unwrap();
        read_codex_request(&mut stream);
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", payload.len(), payload).unwrap();
        drop(stream);
        listener.set_nonblocking(true).unwrap();
        // Leave a replay window open. No failed/ambiguous request may be sent again.
        std::thread::sleep(std::time::Duration::from_millis(100));
        match listener.accept() {
            Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock),
            Ok(_) => panic!("unexpected request replay"),
        }
    });
    let model = OpenResponsesModel::new(
        "loopback-codex",
        format!("http://127.0.0.1:{port}/responses"),
    )
    .codex_http();
    (
        ResponsesBackend::new(model, ModelSettings::default()),
        server,
    )
}

#[test]
fn codex_sse_errors_controls_and_ambiguity_are_not_replayed() {
    for payload in [
            "event: error\ndata: {\"type\":\"error\",\"code\":\"rate_limit_exceeded\",\"message\":\"Bearer SECRET\"}\n\n",
            "data: {\"type\":\"error\",\"status_code\":500,\"error\":{\"code\":\"server_error\",\"message\":\"SECRET\"}}\n\n",
            "data: {\"type\":\"unknown\"}\n\n", "data: {\n\n",
            "event: keepalive\ndata: {\"type\":\"error\",\"message\":\"SECRET\"}\n\n",
            "data: [DONE]\n\n", "event: unknown\n\n",
            "data: {\"type\":\"keepalive\",\"error\":{}}\n\n",
            "data: {\"type\":\"keepalive\"}",
        ] {
            let (backend, server) = loopback_sse(payload.to_string());
            let error = test_runtime().block_on(backend.request(&[ModelRequest::default()], &[])).unwrap_err();
            assert!(error.contains("SSE"), "{error}");
            assert!(!error.contains("SECRET"));
            assert!(error.len() <= crate::redact::MAX_DIAGNOSTIC_BYTES);
            assert_eq!(backend.request_calls(), 1);
            server.join().unwrap();
        }
}

#[test]
fn codex_sse_preserves_progress_but_does_not_claim_partial_success() {
    let wire = codex_turn_sse_payload(0);
    let normal = wire.split_once("\r\n\r\n").unwrap().1;
    let (backend, server) = loopback_sse(format!(
        "{normal}data: {{\"type\":\"error\",\"code\":\"server_error\",\"message\":\"SECRET\"}}\n\n"
    ));
    let error = test_runtime()
        .block_on(backend.request(&[ModelRequest::default()], &[]))
        .unwrap_err();
    assert!(error.contains("accumulated"), "{error}");
    assert!(!error.contains("accumulated 0"), "{error}");
    assert!(!error.contains("SECRET"));
    assert_eq!(backend.request_calls(), 1);
    server.join().unwrap();
}

#[test]
fn codex_sse_explicit_controls_and_normal_completion() {
    let wire = codex_turn_sse_payload(0);
    let normal = wire.split_once("\r\n\r\n").unwrap().1;
    let (backend, server) = loopback_sse(format!(": heartbeat\r\n\r\nevent: keepalive\r\n\r\ndata: {{\"type\":\"keepalive\"}}\r\n\r\n{normal}"));
    let result = test_runtime()
        .block_on(backend.request(&[ModelRequest::default()], &[]))
        .unwrap();
    assert!(!result.text.is_empty());
    assert_eq!(backend.request_calls(), 1);
    server.join().unwrap();
}

#[test]
fn codex_sse_response_failed_is_typed_scrubbed_and_terminal() {
    let request =
        serde_json::from_value(serde_json::json!({"model": "loopback-codex", "input": []}))
            .unwrap();
    let mut response = serdes_ai_responses::types::ResponseObject::in_progress(
        "failed",
        1,
        "loopback-codex",
        &request,
    );
    response.status = serdes_ai_responses::types::ResponseStatus::Failed;
    // Use the owning wire type's deserializer, rather than a parallel guessed shape.
    response.error = Some(
        serde_json::from_value(
            serde_json::json!({"code":"server_error", "message":"Bearer SECRET"}),
        )
        .unwrap(),
    );
    let event = serdes_ai_responses::types::StreamEvent::ResponseFailed {
        sequence_number: 1,
        response,
    };
    let (backend, server) = loopback_sse(format!(
        "data: {}\n\n",
        serde_json::to_string(&event).unwrap()
    ));
    let error = test_runtime()
        .block_on(backend.request(&[ModelRequest::default()], &[]))
        .unwrap_err();
    assert!(error.contains("provider error (server_error)"), "{error}");
    assert!(!error.contains("SECRET"));
    assert_eq!(backend.request_calls(), 1);
    server.join().unwrap();
}

#[test]
fn codex_sse_ambiguous_terminal_status_is_not_success() {
    let wire = codex_turn_sse_payload(0);
    let normal = wire.split_once("\r\n\r\n").unwrap().1;
    let ambiguous = normal
        .lines()
        .map(|line| {
            if let Some(payload) = line.strip_prefix("data: ") {
                if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(payload) {
                    if value["type"] == "response.completed" {
                        value["response"]["status"] = serde_json::json!("failed");
                        return format!("data: {}", serde_json::to_string(&value).unwrap());
                    }
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let (backend, server) = loopback_sse(ambiguous);
    let error = test_runtime()
        .block_on(backend.request(&[ModelRequest::default()], &[]))
        .unwrap_err();
    assert!(error.contains("ambiguous completed response"), "{error}");
    assert_eq!(backend.request_calls(), 1);
    server.join().unwrap();
}

#[test]
fn codex_sse_frames_after_terminal_are_not_silent_success() {
    let wire = codex_turn_sse_payload(0);
    let normal = wire.split_once("\r\n\r\n").unwrap().1;
    for suffix in [
        "data: [DONE]\n\ndata: {\"type\":\"keepalive\"}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"sequence_number\":99,\"item_id\":\"i\",\"output_index\":0,\"content_index\":0,\"delta\":\"late\"}\n\n",
    ] {
        let (backend, server) = loopback_sse(format!("{normal}{suffix}"));
        let error = test_runtime().block_on(backend.request(&[ModelRequest::default()], &[])).unwrap_err();
        assert!(error.contains("after"), "{error}");
        assert!(error.contains("accumulated"), "{error}");
        assert_eq!(backend.request_calls(), 1);
        server.join().unwrap();
    }
}

#[test]
fn codex_sse_nonterminal_lifecycle_contradictions_cannot_be_dropped() {
    use serdes_ai_responses::types::{ErrorBodyRef, ResponseObject, ResponseStatus, StreamEvent};
    let wire = codex_turn_sse_payload(0);
    let normal = wire.split_once("\r\n\r\n").unwrap().1;
    let request =
        serde_json::from_value(serde_json::json!({"model":"loopback-codex","input":[]})).unwrap();
    for created in [true, false] {
        for with_error in [false, true] {
            let mut response = ResponseObject::in_progress("r", 1, "loopback-codex", &request);
            if with_error {
                response.error = Some(ErrorBodyRef {
                    code: "server_error".into(),
                    message: "SECRET".into(),
                });
            } else {
                response.status = ResponseStatus::Failed;
            }
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
            let (backend, server) = loopback_sse(format!(
                "data: {}\n\n{normal}",
                serde_json::to_string(&event).unwrap()
            ));
            let error = test_runtime()
                .block_on(backend.request(&[ModelRequest::default()], &[]))
                .unwrap_err();
            assert!(error.contains("ambiguous"), "{error}");
            assert!(!error.contains("SECRET"));
            assert_eq!(backend.request_calls(), 1);
            server.join().unwrap();
        }
    }
}
