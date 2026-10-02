//! Owning transport regression: evidence escapes once; terminal errors never replay.
use futures::StreamExt;
use serdes_ai_core::messages::ModelRequest;
use serdes_ai_core::ModelSettings;
use serdes_ai_models::model::{Model, ModelRequestParameters};
use serdes_ai_models::ModelError;
use serdes_ai_responses::client::OpenResponsesModel;
use std::io::{Read, Write};

fn serve(body: Vec<u8>) -> (OpenResponsesModel, std::thread::JoinHandle<()>) {
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
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
        // Split a non-ASCII codepoint between HTTP writes. The decoder works on
        // bytes until a complete SSE line, never lossy-decodes network chunks.
        for chunk in body.chunks(7) {
            socket.write_all(chunk).unwrap();
        }
        drop(socket);
        listener.set_nonblocking(true).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
    });
    (
        OpenResponsesModel::new("fixture", format!("http://127.0.0.1:{port}/responses"))
            .codex_http(),
        server,
    )
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
            Ok(_) => count += 1,
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
        let mut byte = [0];
        match socket.read(&mut byte) {
            Ok(0) => {}
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
            other => panic!("cancelled owner left HTTP request alive: {other:?}"),
        }
        listener.set_nonblocking(true).unwrap();
        assert!(matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
    });
    (
        OpenResponsesModel::new("fixture", format!("http://127.0.0.1:{port}/responses"))
            .codex_http(),
        server,
    )
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
    let model = OpenResponsesModel::new("fixture", format!("http://127.0.0.1:{port}/responses"))
        .codex_http();
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
