//! Real CLI and HTTP boundary coverage for opt-in transcript emission.
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::process::{Command, Output};

fn bin(root: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"));
    command.env("LLXPRT_CONFIG_HOME", root);
    command
}

fn run(root: &std::path::Path, emit: &[&str]) -> (Output, Vec<Value>) {
    run_calls(root, emit, vec![read_call("call_9")])
}

fn read_call(id: &str) -> Value {
    json!({"id":id,"type":"function","function":{"name":"read_file","arguments":"{\"path\":\"input.txt\",\"offset\":0,\"limit\":4096,\"max_output_bytes\":4096}"}})
}

fn run_calls(root: &std::path::Path, emit: &[&str], calls: Vec<Value>) -> (Output, Vec<Value>) {
    run_capped(root, emit, calls, "512")
}

fn run_capped(
    root: &std::path::Path,
    emit: &[&str],
    calls: Vec<Value>,
    cap: &str,
) -> (Output, Vec<Value>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for round in 0..2 {
            listener.set_nonblocking(true).unwrap();
            let started = std::time::Instant::now();
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            started.elapsed().as_secs() < 20,
                            "CLI did not send round {round}"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(error) => panic!("accept: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
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
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            requests.push(serde_json::from_slice(&bytes).unwrap());
            let message = if round == 0 {
                json!({"role":"assistant","content":if calls.len() > 1 { "" } else { "inspect first" },"tool_calls":calls})
            } else {
                json!({"role":"assistant","content":"done"})
            };
            let body = json!({"id":"r","object":"chat.completion","created":1,"model":"loopback","choices":[{"index":0,"message":message,"finish_reason":if round==0 {"tool_calls"} else {"stop"}}]}).to_string();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        }
        requests
    });
    std::fs::create_dir_all(root.join("profiles")).unwrap();
    std::fs::write(root.join("profiles/transcript.json"), json!({"provider":"openai","model":"loopback","ephemeralSettings":{"base-url":format!("http://{address}"),"auth-key":"secret-test-key-28"}}).to_string()).unwrap();
    std::fs::write(
        root.join("input.txt"),
        "hello secret-test-key-28\n".repeat(300),
    )
    .unwrap();
    let output = bin(root)
        .args(["--session", "demo", "--profile", "transcript", "--cwd"])
        .arg(root)
        .args(["--max-tool-output", cap, "-p", "inspect input"])
        .args(emit)
        .output()
        .unwrap();
    (output, server.join().unwrap())
}

fn snapshot(path: &std::path::Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            entries.extend(snapshot(&path));
        } else {
            entries.push((path.clone(), std::fs::read(path).unwrap()));
        }
    }
    entries.sort();
    entries
}

#[test]
fn emission_matches_model_ingress_and_read_only_lookback() {
    let temp = tempfile::tempdir().unwrap();
    let memory = temp.path().join("memory.jsonl");
    let (output, requests) = run(
        temp.path(),
        &[
            "--emit",
            "thinking,text",
            "--emit",
            "calls,results",
            "--mem-profile",
            memory.to_str().unwrap(),
        ],
    );
    assert!(memory.is_file());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let _: Value = serde_json::from_slice(&output.stdout).unwrap();
    let events: Vec<Value> = String::from_utf8(output.stderr)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        events
            .iter()
            .map(|event| event["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "assistant_text",
            "tool_call",
            "tool_result",
            "assistant_text"
        ]
    );
    assert_eq!(events[1]["id"], "call_9");
    assert_eq!(events[1]["index"], 0);
    assert_eq!(events[1]["of"], 1);
    assert_eq!(events[3]["round"], 2);
    let result = events[2]["result"].as_str().unwrap();
    assert!(!result.contains("secret-test-key-28"));
    let messages = requests[1]["messages"].as_array().unwrap();
    let model_result = messages
        .iter()
        .find(|message| message["role"] == "tool")
        .unwrap();
    assert_eq!(model_result["content"], result);
    let session_dir = temp.path().join("code-rs-sessions/demo");
    let before = snapshot(&session_dir);
    let lookback = bin(temp.path())
        .args(["transcript", "--session", "demo", "--json"])
        .output()
        .unwrap();
    assert!(
        lookback.status.success(),
        "{}",
        String::from_utf8_lossy(&lookback.stdout)
    );
    assert!(lookback.stderr.is_empty());
    let transcript: Value = serde_json::from_slice(&lookback.stdout).unwrap();
    assert_eq!(
        transcript["turns"][0]["rounds"][0]["calls"][0]["result"],
        result
    );
    assert_eq!(transcript["turns"][0]["rounds"][1]["text"], "done");
    let human = bin(temp.path())
        .args([
            "transcript",
            "--session",
            "demo",
            "--turn",
            "1",
            "--max-bytes",
            "32",
        ])
        .output()
        .unwrap();
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains("Round 1"));
    assert!(human.contains("[truncated]"));
    assert_eq!(before, snapshot(&session_dir));
    let absent_turn = bin(temp.path())
        .args(["transcript", "--session", "demo", "--turn", "9", "--json"])
        .output()
        .unwrap();
    assert_eq!(absent_turn.status.code(), Some(4));
    let _: Value = serde_json::from_slice(&absent_turn.stdout).unwrap();
    assert_eq!(before, snapshot(&session_dir));
}

#[test]
fn bulk_compaction_result_is_the_exact_wire_payload() {
    let root = tempfile::tempdir().unwrap();
    let (output, requests) = run_capped(
        root.path(),
        &["--emit", "results"],
        vec![read_call("bulk")],
        "4096",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let event: Value = serde_json::from_slice(&output.stderr).unwrap();
    let wire = requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool")
        .unwrap();
    assert_eq!(event["result"], wire["content"]);
    assert!(event["result"].as_str().unwrap().starts_with("CTXDIGEST"));
}

#[test]
fn budget_and_unknown_refusals_match_wire_in_execution_order() {
    let root = tempfile::tempdir().unwrap();
    let unknown = json!({"id":"unknown","type":"function","function":{"name":"not_registered","arguments":"{}"}});
    let (output, requests) = run_calls(
        root.path(),
        &["--emit", "all", "--max-tool-calls", "1"],
        vec![unknown, read_call("exec"), read_call("skipped")],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let events: Vec<Value> = String::from_utf8(output.stderr)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let calls: Vec<_> = events.iter().filter(|e| e["type"] == "tool_call").collect();
    assert_eq!(
        calls
            .iter()
            .map(|e| e["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["unknown", "exec", "skipped"]
    );
    for (index, call) in calls.iter().enumerate() {
        assert_eq!(call["index"], index);
        assert_eq!(call["of"], 3);
    }
    let results: Vec<_> = events
        .iter()
        .filter(|e| e["type"] == "tool_result")
        .collect();
    assert_eq!(
        results
            .iter()
            .map(|e| e["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["exec", "skipped", "unknown"]
    );
    let wire: Vec<_> = requests[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "tool")
        .collect();
    assert_eq!(wire.len(), results.len());
    for (result, message) in results.iter().zip(wire) {
        assert_eq!(result["id"], message["tool_call_id"]);
        assert_eq!(result["result"], message["content"]);
    }
    assert_eq!(results[0]["refused"], false);
    assert_eq!(results[1]["refused"], true);
    assert_eq!(results[2]["refused"], true);
    // The tool's framed read uses its existing byte-count truncation marker.
    assert!(results[0]["result"]
        .as_str()
        .unwrap()
        .contains("[truncated "));
    assert!(results[0]["result"].as_str().unwrap().len() < 700);
    assert!(results[0]["result"].as_str().unwrap().contains("budget:"));
}

#[test]
fn chat_thinking_only_is_an_explicit_noop() {
    let root = tempfile::tempdir().unwrap();
    let (output, _) = run(root.path(), &["--emit", "thinking"]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
}

#[test]
fn default_runtime_stderr_is_empty() {
    let temp = tempfile::tempdir().unwrap();
    let (output, _) = run(temp.path(), &[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.stderr.is_empty());
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(envelope.get("transcript").is_none());
}

#[test]
fn missing_session_is_not_created_and_help_teaches() {
    let temp = tempfile::tempdir().unwrap();
    let before = snapshot(temp.path());
    let output = bin(temp.path())
        .args(["transcript", "--session", "missing", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4));
    assert!(output.stderr.is_empty());
    let _: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(before, snapshot(temp.path()));
    let help = bin(temp.path()).arg("--help").output().unwrap();
    let help = String::from_utf8(help.stdout).unwrap();
    for text in [
        "--emit",
        "Chat has no thinking",
        "2>turn.jsonl",
        "transcript --session",
        "2 usage, 3 config, 4 session, 5 model, 6 turn",
    ] {
        assert!(help.contains(text), "missing {text}");
    }
}
