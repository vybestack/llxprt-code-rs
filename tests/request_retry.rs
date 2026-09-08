//! Actual provider mapping + final CLI envelope, confined to a bounded loopback server.
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn run(statuses: &[u16], hint: Option<u64>, malformed: bool) -> (Value, i32, usize) {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let statuses = statuses.to_vec();
    let profile = root.path().join("loop.json");
    std::fs::write(
        &profile,
        json!({
            "provider":"openai", "model":"loopback", "ephemeralSettings": {
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
        for status in statuses {
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < end,
                            "expected provider attempt not received"
                        );
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("loopback accept: {error}"),
                }
            };
            calls += 1;
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = std::io::BufReader::new(&mut stream);
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
            let body = if status == 200 {
                if malformed {
                    "not JSON".into()
                } else {
                    json!({"id":"r", "object":"chat.completion", "created":1,
                        "model":"loopback", "choices":[{"index":0,"message":{"role":"assistant","content":"OK"},"finish_reason":"stop"}],
                        "usage":{"prompt_tokens":0,"completion_tokens":0,"total_tokens":0}
                    }).to_string()
                }
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
        let (value, exit, calls) = run(&[status, 200], hint, false);
        assert_eq!(exit, 0);
        assert_eq!(calls, 2);
        assert_eq!(value["request_attempts"], json!({"attempts":2,"retries":1}));
        assert_eq!(value["tool_calls"], 0);
    }
}

#[test]
fn real_exhaustion_retains_class_counts_and_scrubbed_error() {
    let (value, exit, calls) = run(&[503; 4], None, false);
    assert_eq!(exit, 5);
    assert_eq!(calls, 4);
    assert_eq!(value["request_attempts"], json!({"attempts":4,"retries":3}));
    assert_eq!(value["error"]["code"], "model-transient-server");
}

#[test]
fn real_auth_and_invalid_json_are_not_retried() {
    for (status, malformed) in [(401, false), (200, true)] {
        let (value, exit, calls) = run(&[status], None, malformed);
        assert_eq!(exit, 5);
        assert_eq!(calls, 1);
        assert_eq!(value["request_attempts"], json!({"attempts":1,"retries":0}));
    }
}
