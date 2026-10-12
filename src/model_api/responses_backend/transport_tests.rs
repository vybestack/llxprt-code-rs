//! HTTP status/hint facts survive both retained Responses transports.
use super::*;
use crate::adapter::ChatBackend;
use crate::transport::TransportKind;
use std::io::{BufRead, Read, Write};
use std::net::TcpListener;
use std::time::Duration;

#[test]
fn codex_http_status_facts_survive_diagnostic_body_timeout_and_quota() {
    for streaming in [false, true] {
        for (status, body, incomplete, kind) in [
            (401, "partial", true, TransportKind::Permanent),
            (
                429,
                r#"{"error":{"code":"insufficient_quota","message":"payment required"}}"#,
                false,
                TransportKind::QuotaExhausted,
            ),
            (
                503,
                "temporarily unavailable",
                false,
                TransportKind::TransientServer,
            ),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let (done, release) = std::sync::mpsc::channel();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                drain_request(&stream);
                let declared_length = body.len() + if incomplete { 100 } else { 0 };
                write!(stream, "HTTP/1.1 {status} Fixture\r\nRetry-After: 7\r\nContent-Length: {declared_length}\r\nConnection: close\r\n\r\n{body}").unwrap();
                if incomplete {
                    release.recv_timeout(Duration::from_secs(5)).unwrap();
                }
            });
            let mut model =
                OpenResponsesModel::new("fixture", format!("http://{address}/responses"))
                    .with_http_client(reqwest::Client::builder().no_proxy().build().unwrap());
            if streaming {
                model = model.codex_http();
            }
            let backend = ResponsesBackend::new(
                model,
                ModelSettings {
                    timeout: Some(Duration::from_millis(100)),
                    ..Default::default()
                },
            );
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let error = runtime
                .block_on(backend.request(&[ModelRequest::default()], &[]))
                .unwrap_err();
            let BackendError::Transport(failure) = error else {
                panic!("expected typed status: {error}");
            };
            assert_eq!(failure.kind, kind);
            assert_eq!(failure.status, Some(status));
            assert_eq!(failure.retry_after, Some(Duration::from_secs(7)));
            assert_eq!(backend.stream_interruption(), None);
            assert_eq!(backend.request_calls(), 1);
            if incomplete {
                done.send(()).unwrap();
            }
            server.join().unwrap();
        }
    }
}

fn drain_request(stream: &std::net::TcpStream) {
    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
    let mut length = 0;
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap();
            }
        }
    }
    reader.read_exact(&mut vec![0; length]).unwrap();
}
