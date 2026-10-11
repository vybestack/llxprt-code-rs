//! Issue #215: single-turn recovery at the reported 393216-token guard.
//! Exercise the stock CLI, registered reads, actual wire requests and durable failure.

use llxprt_code_rs::session::{Lifecycle, SessionId, SessionStore};
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::time::{Duration, Instant};

const TOKENS: usize = 393_216;
const GUARD: usize = TOKENS * 3;

fn serve_read(listener: &TcpListener, index: usize, latest_over: bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut socket = loop {
        match listener.accept() {
            Ok((socket, _)) => break socket,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "missing request {index}");
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("accept: {error}"),
        }
    };
    socket.set_nonblocking(false).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut reader = std::io::BufReader::new(socket.try_clone().unwrap());
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
    let length = length.unwrap();
    assert!(length <= GUARD, "over-guard request sent: {length}");
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    let message = if index <= 3 {
        let call = |id: String| {
            json!({"id":id,"type":"function",
            "function":{"name":"read_file","arguments":json!({
                "path":"evidence.txt","limit":GUARD + 1000,
                "max_output_bytes":GUARD + 2000,
            }).to_string()}})
        };
        let calls = if latest_over {
            vec![call("read-1".into()), call("read-2".into())]
        } else {
            vec![call(format!("read-{index}"))]
        };
        json!({"role":"assistant","content":"Read bounded evidence",
            "tool_calls":calls})
    } else {
        json!({"role":"assistant","content":"Evidence inspected; task complete."})
    };
    let finish = if index <= 3 { "tool_calls" } else { "stop" };
    let response = json!({"id":format!("reply-{index}"),"object":"chat.completion",
        "created":1,"model":"fixture","choices":[{"index":0,"message":message,
            "finish_reason":finish}]})
    .to_string();
    write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
    serde_json::from_slice(&body).unwrap()
}

fn run(evidence_bytes: usize, replies: usize) -> (Value, Vec<Value>, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("evidence.txt"), "x".repeat(evidence_bytes)).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let profile = root.path().join("fixture.json");
    std::fs::write(
        &profile,
        json!({"provider":"openai","model":"fixture",
        "ephemeralSettings":{"base-url":format!("http://{}/v1", listener.local_addr().unwrap()),
            "auth-key":"public-issue215-fixture","context-limit":TOKENS}})
        .to_string(),
    )
    .unwrap();
    let server = std::thread::spawn(move || {
        let requests: Vec<_> = (1..=replies)
            .map(|i| serve_read(&listener, i, replies == 1))
            .collect();
        (listener, requests)
    });
    let output = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
        .env("LLXPRT_CONFIG_HOME", root.path())
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env_remove("no_proxy")
        .args(["--profile-load"])
        .arg(profile)
        .args(["--session", "issue215-single-turn", "--cwd"])
        .arg(root.path())
        .args([
            "--max-tool-calls",
            "8",
            "--turn-time",
            "15s",
            "-p",
            "Inspect evidence.txt; preserve these current instructions verbatim.",
        ])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(if replies == 1 { 5 } else { 0 }),
        "{output:?}"
    );
    let terminal: Value = serde_json::from_slice(&output.stdout).unwrap();
    let (listener, requests) = server.join().unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "the CLI must not send another request after its terminal outcome"
    );
    (terminal, requests, root)
}

#[test]
fn issue215_single_turn_observed_results_shrink_at_original_guard() {
    // Each result fits; three live results do not. Earlier results have already
    // been observed, so the owning compactor may project them to admitted digests.
    let (terminal, requests, root) = run(400_000, 4);
    assert_eq!(GUARD, 1_179_648);
    assert_eq!(terminal["status"], "ok");
    assert_eq!(terminal["tool_calls"], 3);
    assert_eq!(terminal["summary"], "Evidence inspected; task complete.");
    let before = requests[2].to_string().len();
    let after = requests[3].to_string().len();
    assert!(
        after < before - 300_000,
        "wire bytes did not shrink: {before} -> {after}"
    );
    let returns: Vec<_> = requests[3]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "tool")
        .collect();
    assert_eq!(returns.len(), 3);
    let ids: Vec<_> = requests[3]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["tool_calls"].as_array())
        .flatten()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["read-1", "read-2", "read-3"]);
    for (index, result) in returns.iter().enumerate() {
        assert_eq!(result["tool_call_id"], format!("read-{}", index + 1));
        let text = result["content"].as_str().unwrap();
        if index < 2 {
            assert!(text.contains("CTXDIGEST v1"));
            assert!(!text.contains(&"x".repeat(1000)));
        } else {
            assert!(
                text.contains(&"x".repeat(400_000)),
                "latest live evidence was lost"
            );
        }
    }
    assert!(requests[3]
        .to_string()
        .contains("preserve these current instructions verbatim"));
    let store = SessionStore::load_at(
        &SessionId::parse("issue215-single-turn").unwrap(),
        root.path(),
    )
    .unwrap();
    let state = store.snapshot().unwrap();
    assert_eq!(
        state.branches.len(),
        1,
        "this must remain a single-turn fixture"
    );
    assert_eq!(state.branches[0].lifecycle, Lifecycle::Completed);
    assert_eq!(state.branches[0].rounds.len(), 4);
    for (index, round) in state.branches[0].rounds[..3].iter().enumerate() {
        assert_eq!(round.calls[0].id, format!("read-{}", index + 1));
        assert!(round.calls[0].result.contains("CTXDIGEST v1"));
        assert!(round.calls[0].result_live.is_empty());
    }
    println!("issue215 recovered wire bytes: {before} -> {after}");
}

#[test]
fn issue215_single_turn_latest_result_cannot_fit_reports_identical_measured_sizes() {
    // No prior turn and no earlier observed round exists. The newest round
    // has two reads (each below the 1 MiB read cap). Its combined evidence
    // must not be silently dropped, retried unchanged, or described as recovery.
    let (terminal, requests, root) = run(GUARD / 2 + 1000, 1);
    assert_eq!(terminal["status"], "error");
    assert_eq!(terminal["error"]["code"], "context-limit");
    assert_eq!(requests.len(), 1);
    let message = terminal["error"]["message"].as_str().unwrap();
    let total: usize = message
        .strip_prefix("estimated request would be ")
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(total > GUARD);
    assert!(message.contains(&format!(
        "({} bytes over the {GUARD}-byte heuristic guard)",
        total - GUARD
    )));
    assert!(message.contains(&format!(
        "still {total} bytes ({} bytes over guard)",
        total - GUARD
    )));
    assert!(message.contains("reduced by 0 bytes"));
    assert!(message.contains("compaction did not shrink the request"));
    assert!(message.contains("no eligible history or observed tool results could be removed"));
    assert!(message.contains("latest tool evidence are retained"));
    assert!(message.contains("no request sent"));
    assert!(
        !message.contains(&"x".repeat(1000)),
        "diagnostic disclosed tool content"
    );
    let store = SessionStore::load_at(
        &SessionId::parse("issue215-single-turn").unwrap(),
        root.path(),
    )
    .unwrap();
    let state = store.snapshot().unwrap();
    assert_eq!(state.branches.len(), 1);
    let branch = &state.branches[0];
    assert_eq!(branch.lifecycle, Lifecycle::Failed);
    assert_eq!(branch.rounds.len(), 1);
    assert_eq!(branch.rounds[0].calls.len(), 2);
    assert_eq!(branch.rounds[0].calls[0].id, "read-1");
    assert_eq!(branch.rounds[0].calls[1].id, "read-2");
    for call in &branch.rounds[0].calls {
        assert!(call.ok);
        assert!(call.result.contains("CTXDIGEST v1"));
        assert!(call.result_live.is_empty());
    }
    assert_eq!(branch.error, message);
    println!("issue215 unrecoverable diagnostic: {message}");
}
