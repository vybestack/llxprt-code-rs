//! Phase-4 provider consistency tests through the *real* SerdeAI adapter (the transport that
//! the CLI uses). A tiny loopback HTTP server feeds canned OpenAI chat-completion responses so the
//! real `ModelAdapter` parsing path runs: raw finish reasons, internal stop/tool consistency, and
//! malformed raw arguments must surface exactly as the module contract states. Nothing here reaches the
//! network outside the loopback server.

use llxprt_code_rs::adapter::{make_adapter, ChatBackend, LlmResult};
use llxprt_code_rs::model::ModelConfig;
use serdes_ai::core::FinishReason;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;

/// A thread that answers a fixed number of chat/completions requests with `body`, then closes.
fn serve(body: String) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(1) {
            let Ok(mut stream) = stream else {
                continue;
            };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            let mut len = 0usize;
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" || h == "\n" {
                    break;
                }
                if let Some(v) = h
                    .split(':')
                    .next()
                    .map(|k| k.trim().to_ascii_lowercase())
                    .filter(|k| k == "content-length")
                {
                    let _ = v;
                }
                if let Some(rest) = h.split_once(':') {
                    if rest.0.trim().eq_ignore_ascii_case("content-length") {
                        len = rest.1.trim().parse().unwrap_or(0);
                    }
                }
            }
            if len > 0 {
                let mut buf = vec![0u8; len];
                let _ = reader.read_exact(&mut buf);
            }
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    addr
}

fn config(addr: std::net::SocketAddr) -> ModelConfig {
    ModelConfig {
        model: "loopback".into(),
        base_url: llxprt_code_rs::profile::RedactedUrl::parse(&format!("http://{addr}/v1"))
            .unwrap(),
        api_key: "loopback-key".into(),
        keyfile_path: None,
        max_output_tokens: None,
        timeout: Some(std::time::Duration::from_secs(30)),
        model_params: None,
        context_limit: None,
    }
}

fn chat_body(finish_reason: &str, calls_json: Option<&str>) -> String {
    let message = match calls_json {
        Some(calls) => {
            format!(r#""message": {{"role":"assistant","content":null,"tool_calls":[{calls}]}}"#)
        }
        None => r#""message": {"role":"assistant","content":"hello"}"#.to_string(),
    };
    let reason = if finish_reason == "null" {
        "null".to_string()
    } else {
        format!("\"{finish_reason}\"")
    };
    format!(
        r#"{{"id":"1","object":"chat.completion","created":1,"model":"loopback","choices":[{{"index":0,{message},"finish_reason":{reason}}}]}}"#
    )
}

fn request_one(cfg: &ModelConfig) -> Result<LlmResult, String> {
    let tools = llxprt_code_rs::tools::tool_specs(false);
    let adapter = make_adapter(cfg).map_err(|e| e.message)?;
    let reqs = vec![llxprt_code_rs::adapter::system_request("s")];
    adapter.request(&reqs, &tools)
}

/// Unknown finish reasons remain typed as unknown even when their raw spelling is accepted for a
/// known reason by another transport.
#[test]
fn unknown_finish_reason_fails() {
    let tool_call = r#"{"id":"c1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"x\"}"}}"#;
    for (reason, calls) in [
        ("weird_reason", None),
        ("end_turn", None),
        ("stop_sequence", None),
        ("tool_call", Some(tool_call)),
    ] {
        let addr = serve(chat_body(reason, calls));
        let got = request_one(&config(addr)).expect("transport ok");
        assert_eq!(
            got.finish_reason,
            Some(FinishReason::Other(reason.to_string())),
            "raw reason preserved as unknown"
        );
        let err = llxprt_code_rs::agent::finish_check(&got).expect_err("unknown reason must fail");
        assert!(err.contains(reason), "{err}");
    }
}

/// `stop` with a tool call is internally inconsistent and must fail before any tool runs.
#[test]
fn stop_with_tool_call_fails() {
    let addr = serve(chat_body(
        "stop",
        Some(
            r#"{"id":"c1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"x\"}"}}"#,
        ),
    ));
    let got = request_one(&config(addr)).expect("transport ok");
    assert_eq!(got.finish_reason, Some(FinishReason::Stop));
    assert_eq!(got.calls.len(), 1);
    let err = llxprt_code_rs::agent::finish_check(&got).expect_err("stop + tool call must fail");
    assert!(err.contains("stop"), "{err}");
}

/// Protocol-invalid response envelopes fail in the provider parser, before a tool call can reach
/// the host executor.
#[test]
fn invalid_response_role_and_tool_type_fail_before_execution() {
    let call = r#"{"id":"c1","type":"function","function":{"name":"write_file","arguments":"{\"path\":\"created.txt\",\"content\":\"must not run\"}"}}"#;
    let invalid_role = chat_body("tool_calls", Some(call)).replacen(
        r#""role":"assistant""#,
        r#""role":"user""#,
        1,
    );
    let invalid_type = chat_body("tool_calls", Some(call)).replacen(
        r#""type":"function""#,
        r#""type":"not_function""#,
        1,
    );

    for body in [invalid_role, invalid_type] {
        let addr = serve(body);
        assert!(
            request_one(&config(addr)).is_err(),
            "invalid provider envelope reached the host"
        );
    }
}

/// `tool_call` with zero calls must fail.
#[test]
fn tool_call_with_no_calls_fails() {
    let addr = serve(chat_body("tool_call", None));
    let got = request_one(&config(addr)).expect("transport ok");
    let err =
        llxprt_code_rs::agent::finish_check(&got).expect_err("tool_call with no calls must fail");
    assert!(err.contains("tool_call"), "{err}");
}

/// A malformed raw argument (invalid JSON) must be preserved as the raw string by the vendored
/// transport and must not parse into a normalized `{}` that could round-trip as success.
#[test]
fn malformed_raw_args_are_preserved() {
    let addr = serve(chat_body(
        "tool_call",
        Some(
            r#"{"id":"c1","type":"function","function":{"name":"read_file","arguments":"{bad json"}}"#,
        ),
    ));
    let got = request_one(&config(addr)).expect("transport ok");
    assert_eq!(got.calls.len(), 1);
    // The raw malformed string is the payload; the adapter never coerced it to `{}`.
    assert!(
        got.calls[0].args_json.contains("bad json"),
        "{:?}",
        got.calls[0].args_json
    );
    // Executing a malformed arg fails, never a successful round.
    let err = llxprt_code_rs::agent::parse_object_args(&got.calls[0]);
    assert!(err.is_err(), "malformed args must be rejected");
}

/// A missing finish_reason must fail.
#[test]
fn missing_finish_reason_fails() {
    let addr = serve(chat_body("null", None));
    let got = request_one(&config(addr)).expect("transport ok");
    assert!(got.finish_reason.is_none());
    let err = llxprt_code_rs::agent::finish_check(&got).expect_err("missing reason must fail");
    assert!(err.contains("missing"), "{err}");
}

/// The accepted endpoint forms all reach a loopback chat-completions request. Each real
/// request path is verified; an arbitrary prefix parses but is rejected by
/// [`ModelConfig::from_profile`], so it never issues a request.
#[test]
fn endpoint_route_matrix_and_loopback_requests() {
    use llxprt_code_rs::model::ModelConfig;
    use llxprt_code_rs::model::ModelError;
    use llxprt_code_rs::profile::EphemeralSettings;
    use llxprt_code_rs::profile::Profile;
    use llxprt_code_rs::profile::RedactedUrl;

    // Accepted base forms derive the same full route.
    for (base, expected) in [
        (
            "http://127.0.0.1:8080",
            "http://127.0.0.1:8080/v1/chat/completions",
        ),
        (
            "http://127.0.0.1:8080/",
            "http://127.0.0.1:8080/v1/chat/completions",
        ),
        (
            "http://127.0.0.1:8080/v1",
            "http://127.0.0.1:8080/v1/chat/completions",
        ),
        (
            "http://127.0.0.1:8080/v1/",
            "http://127.0.0.1:8080/v1/chat/completions",
        ),
        (
            "http://127.0.0.1:8080/chat/completions",
            "http://127.0.0.1:8080/chat/completions",
        ),
        (
            "http://127.0.0.1:8080/v1/chat/completions",
            "http://127.0.0.1:8080/v1/chat/completions",
        ),
    ] {
        assert_eq!(
            llxprt_code_rs::agent::chat_route(base),
            expected,
            "route for {base}"
        );
    }

    // Every accepted form resolves through from_profile and reaches the loopback request path.
    for host_path in [
        "",
        "/",
        "/v1",
        "/v1/",
        "/chat/completions",
        "/v1/chat/completions",
    ] {
        let addr = serve(chat_body("stop", None));
        let base = format!("http://{addr}{host_path}");
        let p = Profile {
            name: "t".into(),
            provider: "openai".into(),
            model: "m".into(),
            model_params: Default::default(),
            ephemeral: EphemeralSettings {
                base_url: Some(RedactedUrl::parse(&base).unwrap()),
                auth_key: Some("k".into()),
                ..Default::default()
            },
        };
        let cfg = ModelConfig::from_profile(&p, true, true)
            .unwrap_or_else(|e| panic!("base {base} must resolve: {e}"));
        // The real adapter reaches the loopback request path for every accepted form.
        let got = request_one(&cfg).expect("loopback request on accepted form");
        assert_eq!(
            got.finish_reason.as_ref(),
            Some(&FinishReason::Stop),
            "accepted loopback request for {base}"
        );
    }

    // An arbitrary path prefix is rejected before any request.
    let p = Profile {
        name: "t".into(),
        provider: "openai".into(),
        model: "m".into(),
        model_params: Default::default(),
        ephemeral: EphemeralSettings {
            base_url: Some(RedactedUrl::parse("http://127.0.0.1:8080/inference/v1").unwrap()),
            auth_key: Some("k".into()),
            ..Default::default()
        },
    };
    let err = ModelConfig::from_profile(&p, true, true).expect_err("arbitrary prefix must reject");

    assert!(
        matches!(err, ModelError::InvalidEndpoint(_)),
        "unexpected: "
    );
    assert!(
        !format!("{err:?}").contains("inference"),
        "no unsanitized path echoed"
    );
}

#[test]
fn request_budget_constants_keep_their_public_agent_paths() {
    assert_eq!(llxprt_code_rs::agent::PER_REQUEST_OVERHEAD_BYTES, 512);
    assert_eq!(llxprt_code_rs::agent::PER_PART_OVERHEAD_BYTES, 128);
}
