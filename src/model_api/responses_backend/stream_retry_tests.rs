//! Real Responses backend + production agent loop; no endpoint-policy relaxation.
use super::*;
use crate::agent::CodingAgent;
use crate::session::{SessionId, SessionStore};
use crate::test_loopback as loopback;
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::net::TcpListener;

fn read_request(stream: &mut loopback::Connection) -> Value {
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
    let (result, requests, transcript, root) = run_detailed(bodies);
    assert_eq!(
        result.is_ok(),
        status == 0,
        "{:?}",
        result.as_ref().err().map(|e| &e.message)
    );
    let status = if result.is_ok() { "ok" } else { "error" };
    (json!({"status":status}), requests, transcript, root)
}
fn run_detailed(
    bodies: Vec<(String, usize)>,
) -> (
    Result<crate::agent::CompletedRun, crate::agent::AgentError>,
    Vec<Value>,
    Value,
    tempfile::TempDir,
) {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = loopback::spawn(listener, move |listener| {
        let mut requests = Vec::new();
        for (body, missing) in bodies {
            let mut stream = listener.accept().unwrap().0;

            requests.push(read_request(&mut stream));
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()+missing).unwrap();
        }
        listener.expect_no_more_requests();
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
    let agent = CodingAgent::with_backend(
        Box::new(crate::test_retry_clock::RetryClock::new(backend)),
        root.path().into(),
        false,
    )
    .with_max_tool_calls(Some(4));
    let id = SessionId::parse("stream-retry").unwrap();
    let store = SessionStore::load_at(&id, root.path()).unwrap();
    let reserved = store
        .start_request(None, None, "Inspect then summarize.", root.path())
        .unwrap();
    let result = agent.run(&store, &reserved);
    let requests = server.join().unwrap();
    let state = SessionStore::read_transcript_at(&id, root.path()).unwrap();
    let transcript = json!({"turns":[{"model_retries":state.branches[0].stream_retries,"rounds":state.branches[0].rounds}]});
    (result, requests, transcript, root)
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
fn unknown_provider_code_reaches_the_headless_error_message_without_replay() {
    for (event, expected) in [
        (
            r#"{"type":"error","status_code":429,"error":{"code":"usage_limit_reached","message":"fixture-only-secret"}}"#,
            "provider error (rate_limit_exceeded; provider_code=usage_limit_reached; status=429); request not replayed",
        ),
        (
            r#"{"type":"error","error":{"code":"brand_new_code","message":"fixture-only-secret","param":"fixture-only-secret"}}"#,
            "provider error (unclassified; provider_code=brand_new_code); request not replayed",
        ),
        (
            r#"{"type":"error","code":"Bearer fixture-only-secret","message":"x"}"#,
            "provider error (unclassified; provider_code=invalid_code); request not replayed",
        ),
    ] {
        let (result, requests, transcript, _) =
            run_detailed(vec![(format!("data: {event}\n\n"), 0)]);
        let Err(error) = result else {
            panic!("provider error is terminal")
        };
        assert_eq!(error.key, "model");
        assert!(error.message.contains(expected), "{}", error.message);
        assert!(!error.message.contains("fixture-only-secret"));
        assert_eq!(requests.len(), 1);
        assert!(transcript["turns"][0]["model_retries"]
            .as_array()
            .unwrap()
            .is_empty());
    }
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

#[test]
fn sse_provider_categories_reach_terminal_json_without_replay() {
    for (provider_code, expected) in [
        ("server_is_overloaded", "model-transient-server"),
        ("server_error", "model-transient-server"),
        ("internal_error", "model-transient-server"),
        ("rate_limit_exceeded", "model-rate-limit"),
        ("rate_limit_error", "model-rate-limit"),
        ("brand_new_code", "model"),
    ] {
        for failed_response in [false, true] {
            let event = if failed_response {
                json!({"type":"response.failed","sequence_number":0,"response":{
                    "id":"failed","object":"response","created_at":1,"model":"fixture",
                    "status":"failed","output":[],
                    "error":{"code":provider_code,"message":"fixture-only-secret"}
                }})
            } else {
                json!({"type":"error","error":{
                    "code":provider_code,"message":"fixture-only-secret"
                }})
            };
            let (result, requests, transcript, _) =
                run_detailed(vec![(format!("data: {event}\n\n"), 0)]);
            let Err(error) = result else {
                panic!("provider failure must stay terminal")
            };
            assert_eq!(error.code as i32, 5);
            assert_eq!(error.retry_attempts, Some(0));
            // The same typed envelope/code selection used by the CLI producer.
            let envelope = crate::envelope::Envelope::error(
                "stream-retry",
                error.envelope_code.unwrap_or(error.key),
                &error.message,
            );
            let terminal: Value = serde_json::from_slice(&envelope.to_line()).unwrap();
            assert_eq!(terminal["status"], "error");
            assert_eq!(terminal["error"]["code"], expected, "{event}: {terminal}");
            assert!(
                error.message.contains("accumulated 0 stream events"),
                "{}",
                error.message
            );
            assert!(error.message.contains("request not replayed"));
            assert!(!error.message.contains("fixture-only-secret"));
            assert_eq!(requests.len(), 1);
            assert!(transcript["turns"][0]["model_retries"]
                .as_array()
                .unwrap()
                .is_empty());
            assert!(transcript["turns"][0]["rounds"]
                .as_array()
                .unwrap()
                .is_empty());
        }
    }
}

#[test]
fn informational_protocol_extensions_do_not_change_completion_or_tools() {
    let baseline = healthy();
    let annotation = json!({"type":"response.annotation.future","sequence_number":1,
        "metadata":{"secret":"fixture-only-secret","type":"error"}});
    let (result, requests, transcript, _) = run_detailed(vec![(
        format!("data: {annotation}\n\n{baseline}data: {annotation}\n\n"),
        0,
    )]);
    let result = result.unwrap();
    assert_eq!(result.retry_attempts, 0);
    assert_eq!(requests.len(), 1);
    assert!(!transcript.to_string().contains("fixture-only-secret"));
    assert!(transcript["turns"][0]["model_retries"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn semantic_protocol_drift_reaches_terminal_json_without_replay_or_partial_tools() {
    for kind in [
        "response.output_future.delta",
        "response.function_call_future.delta",
        "response.content_part.future",
        "response.reasoning_future.delta",
        "response.completed.future",
        "response.failed.future",
        "response.incomplete.future",
        "response.created",
        "response.metadata",
    ] {
        let event = json!({"type":kind,"payload":"fixture-only-secret"});
        let (result, requests, transcript, root) = run_detailed(vec![(
            format!("{}data: {event}\n\n{}", partial(), healthy()),
            0,
        )]);
        let error = result.expect_err("semantic drift must fail closed");
        assert_eq!(error.code as i32, 5);
        assert_eq!(error.retry_attempts, Some(0));
        let envelope = crate::envelope::Envelope::error(
            "stream-retry",
            error.envelope_code.unwrap_or(error.key),
            &error.message,
        );
        let terminal: Value = serde_json::from_slice(&envelope.to_line()).unwrap();
        assert_eq!(terminal["status"], "error");
        assert_eq!(terminal["error"]["code"], "model-protocol-unsupported");
        assert!(error.message.contains(kind), "{}", error.message);
        assert!(error.message.contains("accumulated"), "{}", error.message);
        assert!(!error.message.contains("fixture-only-secret"));
        assert!(!error.message.contains("partial-secret"));
        assert_eq!(requests.len(), 1);
        assert!(!root.path().join("must-not-exist").exists());
        assert!(transcript["turns"][0]["model_retries"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(transcript["turns"][0]["rounds"]
            .as_array()
            .unwrap()
            .is_empty());
    }
}
