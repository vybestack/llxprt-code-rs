use super::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::time::Duration;

/// This bypasses no provider code: only the loopback peer is controlled. A short
/// body exercises reqwest's response-transfer failure, distinct from send/head.
#[test]
fn truncated_body_has_typed_cause_and_never_becomes_completion() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let mut len = 0;
        loop {
            line.clear();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            if line == "\r\n" {
                break;
            }
            if let Some((key, value)) = line.split_once(':') {
                if key.eq_ignore_ascii_case("content-length") {
                    len = value.trim().parse().unwrap();
                }
            }
        }
        reader.read_exact(&mut vec![0; len]).unwrap();
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 999\r\nConnection: close\r\n\r\n{\"private\":\"secret-marker\"").unwrap();
    });
    let backend = AnthropicBackend::new(
        serdes_ai::models::anthropic::AnthropicModel::new("fixture", "key-marker")
            .with_base_url(format!("http://{address}")),
        ModelSettings::new().timeout(Duration::from_secs(2)),
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let error = runtime
        .block_on(backend.request(&[crate::adapter::system_request("secret-prompt")], &[]))
        .unwrap_err();
    server.join().unwrap();
    let failure = crate::transport::TransportFailure::from_message(&error).unwrap();
    // reqwest wraps response chunk failures as decode at this reader boundary.
    assert_eq!(failure.origin, "decode");
    assert_eq!(
        failure.observation.cause,
        Some(serdes_ai::models::error::cause::TerminalCause::UnexpectedEof)
    );
    assert!(failure.observation.cause.is_some());
    assert!(failure.observation.provider_call_ms.is_some());
    assert!(!error.contains("secret-marker"));
    assert!(!error.contains("secret-prompt"));
    assert!(!error.contains("key-marker"));
    assert!(!error.contains(&address.to_string()));
    assert_eq!(backend.request_calls(), 1);
}
