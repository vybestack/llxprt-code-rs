//! Faithful compiled CLI workload at the unchanged 300000-token byte guard.
//! Real registered reads, edit and shell test; loopback source controls model replies.

use llxprt_code_rs::session::{Lifecycle, SessionId, SessionStore};
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::time::{Duration, Instant};

struct Fixture {
    root: tempfile::TempDir,
    server: std::thread::JoinHandle<Vec<Vec<u8>>>,
    address: std::net::SocketAddr,
}

fn tool(id: &str, name: &str, args: Value) -> Value {
    json!({"role":"assistant","content":"Inspecting and implementing the bounded task",
        "tool_calls":[{"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}}]})
}

fn replies() -> Vec<Value> {
    let mut messages = Vec::new();
    for i in 1..=3 {
        messages.push(tool(
            &format!("read-{i}"),
            "read_file",
            json!({
                "path":"evidence.txt", "offset":0, "limit":310000, "max_output_bytes":312000,
            }),
        ));
    }
    messages.push(tool("edit", "replace", json!({
        "path":"task.py", "old_string":"return a - b", "new_string":"return a + b", "expected":1,
    })));
    messages.push(tool(
        "test",
        "run_shell_command",
        json!({
            "command":"python3 test_task.py", "timeout_seconds":10,
        }),
    ));
    messages
        .push(json!({"role":"assistant","content":"Fixed addition; the native tool test passed."}));
    messages.push(tool(
        "restored-read",
        "read_file",
        json!({"path":"task.py","max_output_bytes":512}),
    ));
    messages.push(json!({"role":"assistant","content":"Restored session verified the addition implementation."}));
    messages
}

fn serve(listener: TcpListener) -> Vec<Vec<u8>> {
    listener.set_nonblocking(true).unwrap();
    let mut bodies = Vec::new();
    for (round, message) in replies().into_iter().enumerate() {
        let deadline = Instant::now() + Duration::from_secs(45);
        let mut socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "missing native request {round}");
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("{e}"),
            }
        };
        socket.set_nonblocking(false).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        let mut reader = std::io::BufReader::new(&mut socket);
        let mut length = None;
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            if line == "\r\n" {
                break;
            }
            if let Some(n) = line.to_lowercase().strip_prefix("content-length:") {
                length = Some(n.trim().parse::<usize>().unwrap());
            }
        }
        let length = length.unwrap();
        assert!(
            length <= 900_000,
            "native wire body exceeded guard: {length}"
        );
        let mut bytes = vec![0; length];
        reader.read_exact(&mut bytes).unwrap();
        bodies.push(bytes);
        let finish = if message.get("tool_calls").is_some() {
            "tool_calls"
        } else {
            "stop"
        };
        let response = json!({"id":format!("reply-{round}"),"object":"chat.completion","created":1,"model":"fixture",
            "choices":[{"index":0,"finish_reason":finish,"message":message}],
            "usage":{"prompt_tokens":100,"completion_tokens":10,"total_tokens":110,
                "prompt_tokens_details":{"cached_tokens":20}}}).to_string();
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response.len(), response).unwrap();
    }
    bodies
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("task.py"),
            "def add(a, b):\n    return a - b\n",
        )
        .unwrap();
        std::fs::write(root.path().join("test_task.py"), "from task import add\nassert add(7, 4) == 11\nassert add(-2, 2) == 0\nprint('task tests passed')\n").unwrap();
        // Controlled public evidence, not a historical/private session payload.
        std::fs::write(
            root.path().join("evidence.txt"),
            "record: bounded public evidence\n".repeat(10_000),
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || serve(listener));
        Self {
            root,
            server,
            address,
        }
    }

    fn run(&self, turn: &str, prompt: &str) -> Value {
        let profile = self.root.path().join("loopback.json");
        std::fs::write(&profile, json!({"provider":"openai","model":"fixture","ephemeralSettings":{
            "base-url":format!("http://{}/v1", self.address),"auth-key":"public-loopback-marker","context-limit":300000,
        }}).to_string()).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
            .env("LLXPRT_CONFIG_HOME", self.root.path())
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env_remove("HTTP_PROXY")
            .env_remove("HTTPS_PROXY")
            .env_remove("ALL_PROXY")
            .env_remove("http_proxy")
            .env_remove("https_proxy")
            .env_remove("all_proxy")
            .arg("--profile-load")
            .arg(profile)
            .args(["--session", "effective-native", "--turn", turn, "--cwd"])
            .arg(self.root.path())
            .args([
                "--allow-shell",
                "--max-tool-calls",
                "8",
                "--turn-time",
                "40s",
                "-p",
                prompt,
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(envelope["status"], "ok");
        assert_eq!(envelope["replayed"], false);
        let events: Vec<Value> = String::from_utf8(output.stderr)
            .unwrap()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        let calls: Vec<_> = events
            .iter()
            .filter(|e| e["event"] == "prompt_cache_call")
            .collect();
        assert_eq!(calls.len(), if turn == "1" { 6 } else { 2 });
        for call in calls {
            assert_eq!(call["reported_input_tokens"], 100);
        }
        envelope
    }
}

#[test]
fn compiled_tool_workload_compacts_and_remains_productive_after_restore() {
    let fixture = Fixture::new();
    let first = fixture.run(
        "1",
        "Inspect bounded evidence, repair add, run its test. Current instructions verbatim.",
    );
    assert_eq!(first["tool_calls"], 5);
    let second = fixture.run(
        "2",
        "Verify restored task.py; preserve session permissions.",
    );
    assert_eq!(second["tool_calls"], 1);
    let id = SessionId::parse("effective-native").unwrap();
    let store = SessionStore::load_at(&id, fixture.root.path()).unwrap();
    let state = store.snapshot().unwrap();
    assert_eq!(state.branches.len(), 2);
    for branch in &state.branches {
        assert_eq!(branch.lifecycle, Lifecycle::Completed);
    }
    assert!(state.branches[0].rounds[0].calls[0]
        .result
        .contains("CTXDIGEST v1"));
    assert!(state
        .branches
        .iter()
        .flat_map(|b| &b.rounds)
        .flat_map(|r| &r.calls)
        .all(|c| c.result_live.is_empty()));
    assert!(std::fs::read_to_string(fixture.root.path().join("task.py"))
        .unwrap()
        .contains("return a + b"));
    let bodies = fixture.server.join().unwrap();
    let values: Vec<Value> = bodies
        .iter()
        .map(|b| serde_json::from_slice(b).unwrap())
        .collect();
    println!(
        "native request bytes: {:?}",
        bodies.iter().map(Vec::len).collect::<Vec<_>>()
    );
    assert!(
        bodies[3].len() < bodies[2].len() - 200_000,
        "compaction must shrink actual serialized provider body"
    );
    let messages = values[3]["messages"].as_array().unwrap();
    let returns: Vec<_> = messages.iter().filter(|m| m["role"] == "tool").collect();
    assert_eq!(returns.len(), 3);
    assert!(returns[0]["content"]
        .as_str()
        .unwrap()
        .contains("CTXDIGEST v1"));
    assert!(returns[1]["content"]
        .as_str()
        .unwrap()
        .contains("CTXDIGEST v1"));
    assert!(returns[2]["content"]
        .as_str()
        .unwrap()
        .contains("bounded public evidence"));
    for (index, result) in returns.iter().enumerate() {
        assert_eq!(result["tool_call_id"], format!("read-{}", index + 1));
    }
    for value in &values {
        assert_eq!(value["prompt_cache_key"], "effective-native");
        assert!(value["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["function"]["name"] == "run_shell_command"));
    }
    assert!(values[6]
        .to_string()
        .contains("Fixed addition; the native tool test passed."));
}
