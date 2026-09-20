//! Actual provider mapping + final CLI envelope, confined to a bounded loopback server.
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn run(
    provider: &str,
    statuses: &[u16],
    hint: Option<u64>,
    malformed: bool,
    error_body: Option<&str>,
) -> (Value, i32, usize) {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let statuses = statuses.to_vec();
    let error_body = error_body.map(str::to_owned);
    let anthropic = provider == "anthropic";
    let profile = root.path().join("loop.json");
    std::fs::write(
        &profile,
        json!({
            "provider":provider, "model":"loopback", "ephemeralSettings": {
                "base-url":format!("http://{}", listener.local_addr().unwrap()),
                "auth-key":"loopback-only-marker"
            }
        })
        .to_string(),
    )
    .unwrap();
    let server = std::thread::spawn(move || {
        let end = Instant::now() + Duration::from_secs(15);
        let mut calls = 0;
        let (ready, requests) = std::sync::mpsc::channel();
        for status in statuses {
            let mut stream = loop {
                if let Ok(stream) = requests.try_recv() {
                    break stream;
                }
                assert!(
                    Instant::now() < end,
                    "expected provider attempt not received"
                );
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let ready = ready.clone();
                        // A connection is not an HTTP attempt. Hyper may race pooled
                        // connections; read each independently so an idle connection
                        // cannot block the actual retry queued behind it.
                        std::thread::spawn(move || {
                            stream
                                .set_read_timeout(Some(Duration::from_secs(5)))
                                .unwrap();
                            let mut reader = std::io::BufReader::new(&mut stream);
                            let mut line = String::new();
                            match reader.read_line(&mut line) {
                                Ok(0) => return,
                                Err(error)
                                    if matches!(
                                        error.kind(),
                                        std::io::ErrorKind::WouldBlock
                                            | std::io::ErrorKind::TimedOut
                                    ) =>
                                {
                                    return
                                }
                                result => assert!(result.unwrap() > 0),
                            }
                            let mut length = 0;
                            loop {
                                line.clear();
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
                            ready.send(stream).unwrap();
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("loopback accept: {error}"),
                }
            };
            calls += 1;
            let body = if status == 200 {
                if malformed {
                    "not JSON".into()
                } else if anthropic {
                    json!({"id":"r", "type":"message", "role":"assistant", "model":"loopback",
                        "content":[{"type":"text","text":"OK"}], "stop_reason":"end_turn",
                        "usage":{"input_tokens":0,"output_tokens":0}
                    })
                    .to_string()
                } else {
                    json!({"id":"r", "object":"chat.completion", "created":1,
                        "model":"loopback", "choices":[{"index":0,"message":{"role":"assistant","content":"OK"},"finish_reason":"stop"}],
                        "usage":{"prompt_tokens":0,"completion_tokens":0,"total_tokens":0}
                    }).to_string()
                }
            } else if let Some(body) = &error_body {
                body.clone()
            } else {
                json!({"error":{"message":"Bearer loopback-only-marker"}}).to_string()
            };
            let retry = hint
                .map(|n| format!("Retry-After: {n}\r\n"))
                .unwrap_or_default();
            write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\n{retry}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
        calls
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
        .env_clear()
        .env("LLXPRT_CONFIG_HOME", root.path())
        .args(["--session", "retry", "--turn-time", "12s", "--profile-load"])
        .arg(profile)
        .arg("--cwd")
        .arg(root.path())
        .args(["-p", "Reply OK"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let end = Instant::now() + Duration::from_secs(18);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= end {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("CLI exceeded loopback test bound");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.stderr.is_empty());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("loopback-only-marker"));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let schema: Value =
        serde_json::from_slice(include_bytes!("../docs/envelope.schema.json")).unwrap();
    assert!(jsonschema::draft202012::new(&schema)
        .unwrap()
        .is_valid(&value));
    (value, output.status.code().unwrap(), server.join().unwrap())
}

#[test]
fn real_429_retry_after_and_503_recover_in_final_envelope() {
    for (status, hint) in [(429, Some(1)), (503, None)] {
        let (value, exit, calls) = run("openai", &[status, 200], hint, false, None);
        assert_eq!(exit, 0);
        assert_eq!(calls, 2);
        assert_eq!(value["request_attempts"], json!({"attempts":2,"retries":1}));
        assert_eq!(value["tool_calls"], 0);
    }
}

#[test]
fn real_exhaustion_retains_class_counts_and_scrubbed_error() {
    let (value, exit, calls) = run("openai", &[503; 4], None, false, None);
    assert_eq!(exit, 5);
    assert_eq!(calls, 4);
    assert_eq!(value["request_attempts"], json!({"attempts":4,"retries":3}));
    assert_eq!(value["error"]["code"], "model-transient-server");
}

#[test]
fn real_auth_and_invalid_json_are_not_retried() {
    for (status, malformed) in [(401, false), (200, true)] {
        let (value, exit, calls) = run("openai", &[status], None, malformed, None);
        assert_eq!(exit, 5);
        assert_eq!(calls, 1);
        assert_eq!(value["request_attempts"], json!({"attempts":1,"retries":0}));
    }
}

#[test]
fn anthropic_throttle_and_server_refusals_retry_to_success() {
    for status in [429, 503] {
        let (value, exit, calls) = run("anthropic", &[status, 200], Some(1), false, None);
        assert_eq!(exit, 0, "{value}");
        assert_eq!(calls, 2);
        assert_eq!(value["request_attempts"], json!({"attempts":2,"retries":1}));
    }
}

#[test]
fn provider_quota_and_billing_refusals_are_terminal() {
    for (provider, status, body) in [
        ("openai", 429, r#"{"error":{"code":"insufficient_quota"}}"#),
        ("anthropic", 429, r#"{"error":{"type":"billing_error"}}"#),
        ("anthropic", 400, r#"{"error":{"type":"billing_error"}}"#),
    ] {
        let (value, exit, calls) = run(provider, &[status], Some(1), false, Some(body));
        assert_eq!(exit, 5, "{value}");
        assert_eq!(calls, 1);
        assert_eq!(value["error"]["code"], "model-quota-exhausted");
        assert_eq!(value["request_attempts"], json!({"attempts":1,"retries":0}));
    }
}
