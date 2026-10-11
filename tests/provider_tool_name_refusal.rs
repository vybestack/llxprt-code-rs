//! Real CLI + structured OpenAI replies: provider name fields, not assistant prose.
use llxprt_code_rs::session::{Lifecycle, SessionId, SessionStore};
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::time::{Duration, Instant};

fn tool(id: &str, name: &str, args: &str) -> Value {
    json!({"id":id,"type":"function","function":{"name":name,"arguments":args}})
}

fn completion(text: &str, calls: Vec<Value>) -> Value {
    let finish = if calls.is_empty() {
        "stop"
    } else {
        "tool_calls"
    };
    json!({"id":"fixture","object":"chat.completion","created":1,"model":"loopback",
        "choices":[{"index":0,"message":{"role":"assistant","content":text,"tool_calls":calls},"finish_reason":finish}]})
}

fn serve(listener: TcpListener, replies: Vec<Value>) -> Vec<Value> {
    listener.set_nonblocking(true).unwrap();
    let mut captured = Vec::new();
    for reply in replies {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "CLI never requested the expected reply"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("accept: {error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
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
        let mut body = vec![0; length.unwrap()];
        reader.read_exact(&mut body).unwrap();
        captured.push(serde_json::from_slice(&body).unwrap());
        let body = reply.to_string();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
    }
    captured
}

fn run(replies: Vec<Value>, expected_code: i32) -> (Value, Vec<Value>, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    std::fs::create_dir(root.path().join("profiles")).unwrap();
    std::fs::write(
        root.path().join("profiles/loopback.json"),
        json!({
            "provider":"openai","model":"loopback","ephemeralSettings":{
                "base-url":format!("http://{}", listener.local_addr().unwrap()),
                "auth-key":"configured-fixture-secret"
            }
        })
        .to_string(),
    )
    .unwrap();
    let server = std::thread::spawn(move || serve(listener, replies));
    let output = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
        .env_clear()
        .env("LLXPRT_CONFIG_HOME", root.path())
        .env("NO_PROXY", "127.0.0.1,localhost")
        .args([
            "--profile",
            "loopback",
            "--session",
            "name-refusal",
            "--turn-time",
            "5s",
            "--max-tool-calls",
            "4",
            "--allow-shell",
            "-p",
            "Inspect the workspace, then summarize.",
            "--cwd",
        ])
        .arg(root.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(expected_code), "{output:?}");
    let envelope = serde_json::from_slice(&output.stdout).unwrap();
    let captured = server.join().unwrap();
    (envelope, captured, root)
}

fn store(root: &std::path::Path) -> SessionStore {
    SessionStore::load_at(&SessionId::parse("name-refusal").unwrap(), root).unwrap()
}

#[test]
fn oversized_provider_name_refuses_batch_then_corrects_in_same_turn() {
    let name = "x".repeat(65);
    let refused = completion(
        "",
        vec![
            tool(
                "write",
                "write_file",
                r#"{"path":"must-not-exist","content":"private-arg"}"#,
            ),
            tool("bad", &name, "{}"),
        ],
    );
    let (envelope, requests, root) = run(
        vec![
            refused,
            completion("", vec![tool("read", "list_directory", r#"{"path":"."}"#)]),
            completion("Done.", vec![]),
        ],
        0,
    );
    assert_eq!(envelope["status"], "ok");
    assert_eq!(requests.len(), 3);
    let second = requests[1].to_string();
    assert!(second.contains("previous tool-call reply was refused"));
    assert!(!second.contains(&name));
    assert!(!second.contains("private-arg"));
    assert!(!root.path().join("must-not-exist").exists());
    let snapshot = store(root.path()).snapshot().unwrap();
    assert_eq!(snapshot.branches[0].lifecycle, Lifecycle::Completed);
    let calls = snapshot.branches[0]
        .rounds
        .iter()
        .flat_map(|r| &r.calls)
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "list_directory");
}

#[test]
fn repeated_invalid_name_has_typed_terminal_and_sanitized_source_after_work() {
    let name = format!(
        "bad\n\u{1b} https://user:private-password@host/path?token=private-query {}",
        "界".repeat(200)
    );
    let rejected = || completion("", vec![tool("bad", &name, r#"{"unused":"private-arg"}"#)]);
    let (envelope, requests, root) = run(
        vec![
            completion("", vec![tool("read", "list_directory", r#"{"path":"."}"#)]),
            rejected(),
            rejected(),
        ],
        5,
    );
    assert_eq!(requests.len(), 3);
    assert_eq!(envelope["error"]["code"], "malformed_tool_call");
    assert_eq!(envelope["error"]["terminal_outcome"], "malformed_tool_call");
    let message = envelope["error"]["message"].as_str().unwrap();
    assert!(message.contains("tool name exceeds the 64 byte cap"));
    assert!(message.contains("source=mapped provider tool_calls[0].name"));
    assert!(message.contains(&format!("bytes=0..{}", name.len())));
    assert!(message.contains("[truncated]"));
    assert!(message.contains("\\n\\u001b"));
    assert!(!message.chars().any(char::is_control));
    for private in ["private-password", "private-query", "private-arg"] {
        assert!(!message.contains(private));
    }
    let store = store(root.path());
    let snapshot = store.snapshot().unwrap();
    assert_eq!(snapshot.branches[0].lifecycle, Lifecycle::Failed);
    assert_eq!(snapshot.branches[0].rounds.len(), 1);
    assert!(!store.session_dir().join("tool-in-flight.json").exists());
    let retry = store
        .start_request(
            Some(1),
            None,
            "Inspect the workspace, then summarize.",
            root.path(),
        )
        .unwrap();
    assert!(retry.retry);
}

#[test]
fn ordinary_prose_with_long_identifier_never_becomes_a_tool_name() {
    let text = format!(
        "The identifier {} is an example, not a tool invocation.",
        "identifier".repeat(40)
    );
    let (envelope, requests, root) = run(vec![completion(&text, vec![])], 0);
    assert_eq!(envelope["summary"], text);
    assert_eq!(requests.len(), 1);
    assert_eq!(
        store(root.path()).snapshot().unwrap().branches[0].lifecycle,
        Lifecycle::Completed
    );
}

#[test]
fn secret_in_later_call_prevents_reissue_and_diagnostic_excerpt() {
    let name = "x".repeat(65);
    let (envelope, requests, root) = run(
        vec![completion(
            "",
            vec![
                tool("bad", &name, "{}"),
                tool(
                    "later",
                    "read_file",
                    r#"{"path":"configured-fixture-secret"}"#,
                ),
            ],
        )],
        5,
    );
    assert_eq!(requests.len(), 1);
    assert_eq!(envelope["error"]["code"], "model");
    let message = envelope["error"]["message"].as_str().unwrap();
    assert!(message.contains("configured secret"));
    assert!(!message.contains("configured-fixture-secret"));
    assert!(!message.contains(&name));
    assert!(store(root.path()).snapshot().unwrap().branches[0]
        .rounds
        .is_empty());
}
