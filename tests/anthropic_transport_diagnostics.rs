//! Issue 191: controlled failures through the stock CLI, not evidence of Z.ai cause.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::time::Duration;

fn accept(listener: &TcpListener) -> (TcpStream, std::net::SocketAddr) {
    listener.set_nonblocking(true).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match listener.accept() {
            Ok(connection) => {
                // macOS may inherit the listener's nonblocking mode. Fixture reads
                // use bounded blocking I/O, independent of accept polling.
                connection.0.set_nonblocking(false).unwrap();
                return connection;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "CLI never reached provider"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("accept: {error}"),
        }
    }
}

fn read_request(stream: &TcpStream) -> Value {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert!(line.starts_with("POST /v1/messages "));
    let mut length = None;
    loop {
        line.clear();
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
    let mut body = vec![0; length.unwrap()];
    reader.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

fn response(content: Value, stop: &str) -> Vec<u8> {
    let body = json!({"id":"msg", "type":"message", "role":"assistant", "model":"fixture",
        "content":content,"stop_reason":stop,"usage":{"input_tokens":9,"output_tokens":4}})
    .to_string();
    format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).into_bytes()
}

fn invoke(
    address: std::net::SocketAddr,
    root: &std::path::Path,
    session: &str,
) -> std::process::Output {
    let profile = root.join("profile.json");
    std::fs::write(&profile, json!({"provider":"anthropic","model":"fixture",
        "ephemeralSettings":{"base-url":format!("http://{address}"),"auth-key":"issue191-secret-marker",
        "stream-first-response-timeout-ms":1200}}).to_string()).unwrap();
    Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
        .env("LLXPRT_CONFIG_HOME", root).env_remove("LLXPRT_CONFIG_DIR")
        .env_remove("LLXPRT_CONFIG_OVERRIDES").env_remove("LLXPRT_CODE_RS_BIN")
        .args(["--profile-load"]).arg(profile)
        .args(["--session",session,"--cwd"]).arg(root)
        .args(["--allow-shell","--max-tool-calls","8","--turn-time","30s","-p",
            "Implement add(a,b) in arithmetic.py, write test_arithmetic.py with an assertion, run python3 test_arithmetic.py, then summarize."])
        .output().unwrap()
}

/// The envelope carries the rendered diagnostic, so read the measured timing from it.
fn provider_call_ms(message: &str) -> u64 {
    message
        .split_once("provider-call-ms ")
        .and_then(|(_, rest)| rest.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|digits| digits.parse().ok())
        .unwrap_or_else(|| panic!("no provider-call-ms in {message}"))
}

#[test]
fn incomplete_head_after_implementation_edit_preserves_cause_and_timing_without_replay() {
    accepted_loss(
        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n",
        "request",
        "incomplete-message",
        "issue191-incomplete",
    );
}

#[test]
fn truncated_body_after_implementation_edit_is_not_replayed() {
    accepted_loss(
        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 999\r\nConnection: close\r\n\r\n{\"private\":\"secret-marker\"",
        "decode",
        "unexpected-eof",
        "issue363-body",
    );
}

fn accepted_loss(head: &'static [u8], origin: &str, cause: &str, session_id: &str) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut first, _) = accept(&listener);
        let request = read_request(&first);
        assert!(request["tools"].as_array().unwrap().len() >= 6);
        first
            .write_all(&response(
                json!([{"type":"tool_use","id":"write","name":"write_file",
            "input":{"path":"arithmetic.py","content":"def add(a, b):\n    return a + b\n"}}]),
                "tool_use",
            ))
            .unwrap();
        drop(first);
        let (mut second, _) = accept(&listener);
        let request = read_request(&second);
        assert!(request["messages"].to_string().contains("tool_result"));
        // Accepted request, delayed partial response head, then orderly peer closure.
        std::thread::sleep(Duration::from_millis(60));
        second.write_all(head).unwrap();
        drop(second);
        listener.set_nonblocking(true).unwrap();
        std::thread::sleep(Duration::from_millis(800));
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
            "no automatic accepted-request replay"
        );
    });
    let root = tempfile::tempdir().unwrap();
    let out = invoke(address, root.path(), session_id);
    server.join().unwrap();
    assert_eq!(out.status.code(), Some(5));
    let envelope: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(envelope["status"], "error");
    assert_eq!(envelope["error"]["code"], "model-connectivity");
    let message = envelope["error"]["message"].as_str().unwrap();
    assert!(message.contains(&format!("origin {origin}")), "{message}");
    assert!(message.contains(&format!("cause {cause}")), "{message}");
    assert!(message.contains("request not replayed"), "{message}");
    assert_eq!(envelope["error"]["retry_attempts"], 0);
    assert!(provider_call_ms(message) >= 50);
    assert!(message.len() < llxprt_code_rs::transport::MAX_TRANSPORT_DIAGNOSTIC_BYTES);
    for private in [
        "issue191-secret-marker",
        "secret-marker",
        &address.to_string(),
        "arithmetic.py",
        "Content-Type",
    ] {
        assert!(
            !message.contains(private),
            "public diagnostic leaked {private}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(root.path().join("arithmetic.py")).unwrap(),
        "def add(a, b):\n    return a + b\n"
    );
    // Diagnostics persist on the same production path without source strings or headers.
    let session = root.path().join("code-rs-sessions").join(session_id);
    assert!(session.exists());
    let state = llxprt_code_rs::session::SessionStore::read_transcript_at(
        &llxprt_code_rs::session::SessionId::parse(session_id).unwrap(),
        root.path(),
    )
    .unwrap();
    assert_eq!(
        state.branches[0].lifecycle,
        llxprt_code_rs::session::Lifecycle::Failed
    );
    let persisted = &state.branches[0].error;
    assert!(persisted.contains(&format!("cause {cause}")));
    assert!(persisted.contains("provider-call-ms"));
    assert!(persisted.contains("request not replayed"));
    assert!(!persisted.contains("issue191-secret-marker"));
}

#[test]
fn local_request_timeout_is_not_classified_as_incomplete_message() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (stream, _) = accept(&listener);
        read_request(&stream);
        // No response is not proof that the accepted request produced no output.
        std::thread::sleep(Duration::from_millis(1800));
        drop(stream);
        listener.set_nonblocking(true).unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
            "no automatic accepted-request replay"
        );
    });
    let root = tempfile::tempdir().unwrap();
    let out = invoke(address, root.path(), "issue191-timeout");
    server.join().unwrap();
    assert_eq!(out.status.code(), Some(5));
    let envelope: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(envelope["error"]["code"], "model-connectivity");
    let message = envelope["error"]["message"].as_str().unwrap();
    assert!(message.contains("origin timeout"), "{message}");
    assert!(!message.contains("incomplete-message"));
    assert!(message.contains("request not replayed"), "{message}");
    assert_eq!(envelope["error"]["retry_attempts"], 0);
    assert!(provider_call_ms(message) >= 1000);
    assert!(!root.path().join("arithmetic.py").exists());
}

#[test]
fn controlled_implementation_mission_completes_and_its_edits_and_tests_are_inspected() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let actions = [
            (
                "write_file",
                json!({"path":"arithmetic.py","content":"def add(a, b):\n    return a + b\n"}),
            ),
            (
                "write_file",
                json!({"path":"test_arithmetic.py","content":"from arithmetic import add\nassert add(2, 3) == 5\nassert add(-3, 3) == 0\n"}),
            ),
            (
                "run_shell_command",
                json!({"command":"python3 test_arithmetic.py","timeout_seconds":5}),
            ),
        ];
        for (index, (name, input)) in actions.into_iter().enumerate() {
            let (mut stream, _) = accept(&listener);
            read_request(&stream);
            stream
                .write_all(&response(
                    json!([{"type":"tool_use","id":format!("call{index}"),
                "name":name,"input":input}]),
                    "tool_use",
                ))
                .unwrap();
        }
        let (mut stream, _) = accept(&listener);
        let request = read_request(&stream);
        assert!(request["messages"].to_string().contains("tool_result"));
        stream
            .write_all(&response(
                json!([{"type":"text","text":"Implemented addition; both assertions passed."}]),
                "end_turn",
            ))
            .unwrap();
    });
    let root = tempfile::tempdir().unwrap();
    let out = invoke(address, root.path(), "issue191-controlled-mission");
    server.join().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let envelope: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(envelope["status"], "ok");
    assert_eq!(envelope["tool_calls"], 3);
    assert_eq!(
        std::fs::read_to_string(root.path().join("arithmetic.py")).unwrap(),
        "def add(a, b):\n    return a + b\n"
    );
    let test = std::fs::read_to_string(root.path().join("test_arithmetic.py")).unwrap();
    assert!(test.contains("assert add(2, 3) == 5"));
    assert!(Command::new("python3")
        .arg("test_arithmetic.py")
        .current_dir(root.path())
        .status()
        .unwrap()
        .success());
}
