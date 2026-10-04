//! Externally supplied valid u64 counters cannot interrupt tool/terminal persistence.
use llxprt_code_rs::session::{Lifecycle, SessionId, SessionStore};
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn aggregate_overflow_keeps_one_terminal_envelope_and_replayable_tool_history() {
    for off in [false, true] {
        for cached_overflow in [false, true] {
            run_case(off, cached_overflow);
        }
    }
}

fn run_case(off: bool, cached_overflow: bool) {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    std::fs::create_dir_all(root.join("tmp")).unwrap();
    let work = tempfile::tempdir_in(root.join("tmp")).unwrap();
    std::fs::create_dir(work.path().join("profiles")).unwrap();
    std::fs::write(work.path().join("fixture.txt"), "overflow tool evidence").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let mut bodies = Vec::new();
        for round in 0..2 {
            let deadline = Instant::now() + Duration::from_secs(30);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "missing request {round}");
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
            let mut bytes = vec![0; length.unwrap()];
            reader.read_exact(&mut bytes).unwrap();
            bodies.push(serde_json::from_slice::<Value>(&bytes).unwrap());
            let input = if round == 0 { u64::MAX } else { 1 };
            let read = if cached_overflow { input } else { 0 };
            let message = if round == 0 {
                json!({"role":"assistant","content":null,"tool_calls":[{"id":"t","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"fixture.txt\"}"}}]})
            } else {
                json!({"role":"assistant","content":"done"})
            };
            let reply = json!({"id":"chat","object":"chat.completion","created":1,"model":"fixture",
                "choices":[{"index":0,"finish_reason":if round == 0 {"tool_calls"} else {"stop"},"message":message}],
                "usage":{"prompt_tokens":input,"completion_tokens":0,"total_tokens":input,
                "prompt_tokens_details":{"cached_tokens":read}}}).to_string();
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", reply.len(), reply).unwrap();
        }
        bodies
    });
    let mut ephemeral = json!({"base-url":format!("http://{address}/v1"),"auth-key":"fixture-key"});
    if off {
        ephemeral["prompt-caching"] = json!("off");
    }
    std::fs::write(
        work.path().join("profiles/cache.json"),
        json!({"provider":"openai","model":"fixture","ephemeralSettings":ephemeral}).to_string(),
    )
    .unwrap();
    let invoke = || {
        Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
            .env("LLXPRT_CONFIG_HOME", work.path())
            // Only the fixture subprocess ignores off-box proxies.
            .env_remove("HTTP_PROXY")
            .env_remove("HTTPS_PROXY")
            .env_remove("ALL_PROXY")
            .env_remove("http_proxy")
            .env_remove("https_proxy")
            .env_remove("all_proxy")
            .args([
                "--profile",
                "cache",
                "--session",
                "overflow",
                "--turn",
                "1",
                "--cwd",
            ])
            .arg(work.path())
            .arg("--cache-observations")
            .arg(work.path().join("cache.jsonl"))
            .args(["-p", "Read fixture.txt then done"])
            .output()
            .unwrap()
    };
    let output = invoke();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // from_slice rejects an additional object or any other stdout contamination.
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    let schema: Value =
        serde_json::from_slice(include_bytes!("../docs/envelope.schema.json")).unwrap();
    assert!(jsonschema::validator_for(&schema)
        .unwrap()
        .is_valid(&envelope));
    assert_eq!(envelope["status"], "ok");
    assert_eq!(envelope["summary"], "done");
    assert_eq!(envelope["tool_calls"], 1);
    assert_eq!(envelope["replayed"], false);
    assert!(output.stderr.is_empty());
    let events: Vec<Value> = std::fs::read_to_string(work.path().join("cache.jsonl"))
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect();
    let calls: Vec<_> = events
        .iter()
        .filter(|e| e["event"] == "prompt_cache_call")
        .collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["reported_input_tokens"], u64::MAX);
    assert_eq!(calls[1]["reported_input_tokens"], 1);
    assert_eq!(
        calls[0]["cached_input_tokens"],
        if cached_overflow { u64::MAX } else { 0 }
    );
    assert_eq!(calls[1]["cached_input_tokens"], u64::from(cached_overflow));
    let runs: Vec<_> = events
        .iter()
        .filter(|e| e["event"] == "prompt_cache_run")
        .collect();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0]["usage"]["aggregate_valid"], true);
    assert_eq!(runs[1]["usage"]["calls"], 2);
    assert_eq!(runs[1]["usage"]["measured_calls"], 2);
    assert_eq!(runs[1]["usage"]["aggregate_valid"], false);
    assert!(runs[1]["usage"]["measured_input_tokens"].is_null());
    assert_eq!(
        runs[1]["usage"]["measured_cached_tokens"],
        if cached_overflow {
            Value::Null
        } else {
            json!(0)
        }
    );
    assert!(runs[1]["usage"]["hit_ratio"].is_null());
    let bodies = server.join().unwrap();
    assert_eq!(bodies.len(), 2);
    assert!(bodies[1].to_string().contains("overflow tool evidence"));
    for body in &bodies {
        assert_eq!(
            body.get("prompt_cache_key").and_then(Value::as_str),
            if off { None } else { Some("overflow") }
        );
    }
    let store = SessionStore::load_at(&SessionId::parse("overflow").unwrap(), work.path()).unwrap();
    let state = store.snapshot().unwrap();
    assert_eq!(state.branches.len(), 1);
    let branch = &state.branches[0];
    assert_eq!(branch.lifecycle, Lifecycle::Completed);
    assert_eq!(branch.summary, "done");
    assert_eq!(branch.rounds.len(), 2);
    assert_eq!(branch.rounds[0].calls.len(), 1);
    assert_eq!(branch.rounds[0].calls[0].name, "read_file");
    assert!(branch.rounds[0].calls[0].ok);
    assert!(branch.rounds[0].calls[0]
        .result
        .contains("overflow tool evidence"));
    assert_eq!(branch.rounds[1].assistant, "done");
    assert!(branch.rounds[1].calls.is_empty());
    let persisted = serde_json::to_vec(&state).unwrap();
    std::fs::remove_file(work.path().join("cache.jsonl")).unwrap();
    let replay = invoke();
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    let replay_envelope: Value = serde_json::from_slice(&replay.stdout).unwrap();
    assert_eq!(replay_envelope["replayed"], true);
    assert_eq!(replay_envelope["tool_calls"], 1);
    assert_eq!(replay_envelope["summary"], "done");
    assert!(!String::from_utf8(replay.stderr)
        .unwrap()
        .contains("prompt_cache_"));
    assert_eq!(
        serde_json::to_vec(&store.snapshot().unwrap()).unwrap(),
        persisted
    );
    assert_eq!(
        std::fs::read_to_string(work.path().join("fixture.txt")).unwrap(),
        "overflow tool evidence"
    );
    assert!(std::fs::read(work.path().join("cache.jsonl"))
        .unwrap()
        .is_empty());
    std::fs::remove_file(work.path().join("cache.jsonl")).unwrap();
    assert_eq!(std::fs::read_dir(work.path()).unwrap().count(), 3);
}
