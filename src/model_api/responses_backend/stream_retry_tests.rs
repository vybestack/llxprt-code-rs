//! Real Responses backend + production agent loop; no endpoint-policy relaxation.
use super::*;
use crate::agent::CodingAgent;
use crate::session::{SessionId, SessionStore};
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

fn read_request(stream: &mut std::net::TcpStream) -> Value {
    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
    let mut length = None;
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("content-length") {
                length = Some(value.trim().parse::<usize>().unwrap());
            }
        }
    }
    let mut bytes = vec![0; length.unwrap()];
    reader.read_exact(&mut bytes).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
fn run(bodies: Vec<(String, usize)>, status: i32) -> (Value, Vec<Value>, Value, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let mut requests = Vec::new();
        for (body, missing) in bodies {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "missing request");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) => panic!("{e}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            requests.push(read_request(&mut stream));
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()+missing).unwrap();
        }
        std::thread::sleep(Duration::from_millis(100));
        let error = listener.accept().expect_err("unexpected retry");
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        requests
    });
    let model = OpenResponsesModel::new("fixture", format!("http://{address}/responses"))
        .codex_http()
        .with_http_client(reqwest::Client::builder().no_proxy().build().unwrap());
    let backend = crate::model_api::cache_observation::ObservedBackend::new(
        Box::new(
            ResponsesBackend::new(model, ModelSettings::default())
                .with_secrets(vec!["fixture-only-secret".into()]),
        ),
        crate::model_api::cache_observation::InputAccounting::Inclusive,
    );
    let agent = CodingAgent::with_backend(Box::new(backend), root.path().into(), false)
        .with_turn_time(Some(Duration::from_secs(8)))
        .with_max_tool_calls(Some(4));
    let id = SessionId::parse("stream-retry").unwrap();
    let store = SessionStore::load_at(&id, root.path()).unwrap();
    let reserved = store
        .start_request(None, None, "Inspect then summarize.", root.path())
        .unwrap();
    let result = agent.run(&store, &reserved);
    assert_eq!(result.is_ok(), status == 0, "{result:?}");
    let requests = server.join().unwrap();
    let state = SessionStore::read_transcript_at(&id, root.path()).unwrap();
    let transcript = json!({"turns":[{"model_retries":state.branches[0].stream_retries,"rounds":state.branches[0].rounds}]});
    let status = if result.is_ok() { "ok" } else { "error" };
    (json!({"status":status}), requests, transcript, root)
}

fn healthy() -> String {
    let wire = super::tests::codex_turn_sse_payload(0);
    wire.split_once("\r\n\r\n").unwrap().1.to_string()
}
fn partial() -> String {
    let body = healthy();
    let created = body
        .lines()
        .find(|line| line.starts_with("data: {\"type\":\"response.created"))
        .unwrap();
    format!("{created}\n\n{}", [
        json!({"type":"response.output_item.added","sequence_number":1,"output_index":0,"item":{"type":"function_call","id":"item","call_id":"call","name":"write_file","arguments":"","status":"in_progress"}}),
        json!({"type":"response.function_call_arguments.delta","sequence_number":2,"output_index":0,"item_id":"item","delta":r#"{"path":"must-not-exist","content":"partial-secret"}"#}),
    ].iter().map(|v|format!("data: {v}\n\n")).collect::<String>())
}
#[test]
fn interrupted_tool_reply_is_discarded_and_identical_request_recovers() {
    for missing in [0, 999] {
        let (envelope, requests, transcript, root) =
            run(vec![(partial(), missing), (healthy(), 0)], 0);
        assert_eq!(envelope["status"], "ok");
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0], requests[1]);
        assert!(!root.path().join("must-not-exist").exists());
        assert!(!requests[1].to_string().contains("partial-secret"));
        let retries = transcript["turns"][0]["model_retries"].as_array().unwrap();
        assert_eq!(retries.len(), 1);
        assert_eq!(retries[0]["retry"], 1);
        assert_eq!(retries[0]["delay_ms"], 250);
        assert!(retries[0]["events"].as_u64().unwrap() > 0);
        assert!(!transcript.to_string().contains("partial-secret"));
    }
}
#[test]
fn lifecycle_provider_and_post_terminal_failures_never_retry() {
    let failures = [
        "data: {\"type\":\"error\",\"code\":\"server_error\",\"message\":\"provider error\"}\n\n".to_string(),
        "data: {\"type\":\"error\",\"code\":\"rate_limit_exceeded\",\"message\":\"provider throttle\"}\n\n".to_string(),
        "data: {\"type\":\"error\",\"code\":\"insufficient_quota\",\"message\":\"do not retry\"}\n\n".to_string(),
        "data: {\"type\":\"response.function_call_arguments.delta\",\"sequence_number\":0,\"output_index\":0,\"item_id\":\"wrong\",\"delta\":\"{}\"}\n\n".to_string(),
    ];
    for body in failures {
        let (envelope, requests, transcript, _) = run(vec![(body, 0)], 4);
        assert_eq!(envelope["status"], "error");
        assert_eq!(requests.len(), 1);
        assert!(transcript["turns"][0]["model_retries"]
            .as_array()
            .unwrap()
            .is_empty());
    }
    let (_, requests, transcript, _) = run(vec![(healthy(), 999)], 4);
    assert_eq!(requests.len(), 1);
    assert!(transcript["turns"][0]["model_retries"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn repeated_http_interruptions_exhaust_with_three_durable_retries_and_no_tools() {
    let (envelope, requests, transcript, root) = run(vec![(partial(), 999); 4], 4);
    assert_eq!(envelope["status"], "error");
    assert_eq!(requests.len(), 4);
    assert!(requests.windows(2).all(|pair| pair[0] == pair[1]));
    assert_eq!(
        transcript["turns"][0]["model_retries"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert!(transcript["turns"][0]["rounds"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(!root.path().join("must-not-exist").exists());
}
