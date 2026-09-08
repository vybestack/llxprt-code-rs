//! Responses reasoning is emitted only when provided, and never persisted.
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::process::Command;

#[test]
fn responses_thinking_is_bounded_scrubbed_live_only() {
    run_reasoning(
        format!("secret-thinking-key {}", "界".repeat(400_000)),
        None,
    );
}

#[test]
fn reasoning_secret_crossing_adapter_cap_has_no_visible_prefix() {
    let cap = llxprt_code_rs::agent::MAX_TURN_ASSISTANT_BYTES;
    run_reasoning(
        format!(
            "{}secret-thinking-key{}",
            "x".repeat(cap - 18),
            "z".repeat(100)
        ),
        Some("secret-"),
    );
}

fn run_reasoning(thinking: String, forbidden: Option<&str>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(20)))
            .unwrap();
        let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap();
                }
            }
        }
        reader.read_exact(&mut vec![0; length]).unwrap();
        let body = json!({"id":"r","object":"response","created_at":1,"model":"loopback","status":"completed","output":[
            {"id":"reason","type":"reasoning","status":"completed","summary":[{"type":"summary_text","text":thinking}]},
            {"id":"message","type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":"done"}]}
        ]}).to_string();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
    });
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("profiles")).unwrap();
    std::fs::write(root.path().join("profiles/reason.json"), json!({"provider":"openai-responses","model":"loopback","ephemeralSettings":{"base-url":format!("http://{address}/v1/responses"),"auth-key":"secret-thinking-key","reasoning.enabled":true,"reasoning.effort":"high","reasoning.summary":"auto"}}).to_string()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
        .env("LLXPRT_CONFIG_HOME", root.path())
        .args(["--session", "reason", "--profile", "reason", "--cwd"])
        .arg(root.path())
        .args(["--emit", "all", "-p", "hello"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    server.join().unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stderr.contains("secret-thinking-key"));
    if let Some(forbidden) = forbidden {
        assert!(
            !stderr.contains(forbidden),
            "partial secret escaped the adapter cap"
        );
    }
    let events: Vec<Value> = stderr
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["type"], "thinking");
    assert_eq!(events[0]["turn"], 1);
    assert_eq!(events[0]["round"], 1);
    assert_eq!(events[1]["type"], "assistant_text");
    let bytes: usize = events
        .iter()
        .map(|event| event["text"].as_str().unwrap().len())
        .sum();
    assert!(bytes <= llxprt_code_rs::agent::MAX_TURN_ASSISTANT_BYTES);
    assert!(events[0]["text"].as_str().unwrap().contains("[truncated]"));
    let id = llxprt_code_rs::session::SessionId::parse("reason").unwrap();
    let state =
        llxprt_code_rs::session::SessionStore::read_transcript_at(&id, root.path()).unwrap();
    assert_eq!(state.branches[0].rounds[0].assistant, "done");
    assert!(!serde_json::to_string(&state).unwrap().contains('界'));
}
