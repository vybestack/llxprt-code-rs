//! Stock compiled CLI telemetry, persistence, replay, retries and private JSONL ownership.
#[path = "../src/test_loopback.rs"]
mod loopback;
use loopback::Connection as TcpStream;
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

fn command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"));
    command
        .env("LLXPRT_CONFIG_HOME", root)
        .env("NO_PROXY", "127.0.0.1,localhost")
        .args(["--session", "telemetry", "--profile", "telemetry", "--cwd"])
        .arg(root)
        .args(["-p", "inspect input then finish"]);
    command
}

fn profile(root: &Path, address: std::net::SocketAddr, provider: &str) {
    std::fs::create_dir_all(root.join("profiles")).unwrap();
    std::fs::write(root.join("input"), "bounded evidence").unwrap();
    std::fs::write(
        root.join("profiles/telemetry.json"),
        json!({
            "provider":provider, "model":"fixture", "ephemeralSettings":{
                "base-url":format!("http://{address}/v1"), "auth-key":"telemetry-private-secret"
            }
        })
        .to_string(),
    )
    .unwrap();
}

fn read_request(stream: &mut TcpStream) -> Value {
    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
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
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn reply(provider: &str, tool: bool) -> Value {
    match provider {
        "anthropic" => json!({"id":"msg","type":"message","role":"assistant","model":"fixture",
            "stop_reason":if tool {"tool_use"} else {"end_turn"},
            "content":if tool { json!([{"type":"tool_use","id":"read","name":"read_file","input":{"path":"input"}}]) }
                else { json!([{"type":"text","text":"done"}]) },
            "usage":{"input_tokens":20,"output_tokens":5,"cache_creation_input_tokens":30,"cache_read_input_tokens":50}}),
        "openai-responses" => {
            json!({"id":"resp","object":"response","created_at":1,"model":"fixture","status":"completed",
            "output":if tool { json!([{"id":"call","type":"function_call","status":"completed","call_id":"read","name":"read_file","arguments":"{\"path\":\"input\"}"}]) }
                else { json!([{"id":"text","type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":"done"}]}]) },
            "usage":{"input_tokens":100,"output_tokens":5,"total_tokens":105,"input_tokens_details":{"cached_tokens":50},"output_tokens_details":{"reasoning_tokens":2}}})
        }
        _ => json!({"id":"chat","object":"chat.completion","created":1,"model":"fixture",
            "choices":[{"index":0,"finish_reason":if tool {"tool_calls"} else {"stop"},
                "message":if tool {json!({"role":"assistant","content":"inspect","tool_calls":[{"id":"read","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"input\"}"}}]})}
                    else {json!({"role":"assistant","content":"done"})}}],
            "usage":{"prompt_tokens":100,"completion_tokens":5,"total_tokens":105,"prompt_tokens_details":{"cached_tokens":50},"completion_tokens_details":{"reasoning_tokens":2}}}),
    }
}

fn send(stream: &mut TcpStream, value: &Value) {
    let body = value.to_string();
    // Headers and body separated by a measured wait: tests catch "TTFT=completion".
    write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).unwrap();
    stream.flush().unwrap();
    std::thread::sleep(Duration::from_millis(30));
    stream.write_all(body.as_bytes()).unwrap();
}

fn envelope(output: &Output) -> Value {
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("telemetry-private-secret"));
    let schema: Value = serde_json::from_slice(&llxprt_code_rs::envelope::schema_bytes()).unwrap();
    assert!(jsonschema::draft202012::new(&schema)
        .unwrap()
        .is_valid(&value));
    value
}

fn persisted(root: &Path) -> Value {
    let state = llxprt_code_rs::session::SessionStore::read_transcript_at(
        &llxprt_code_rs::session::SessionId::parse("telemetry").unwrap(),
        root,
    )
    .unwrap();
    serde_json::to_value(state.branches.last().unwrap().telemetry.as_ref().unwrap()).unwrap()
}

#[test]
fn compiled_provider_calls_tools_persist_and_replay_without_fabricated_generation() {
    for provider in ["openai", "openai-responses", "anthropic"] {
        let work = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        profile(work.path(), listener.local_addr().unwrap(), provider);
        let server = loopback::spawn(listener, move |listener| {
            for tool in [true, false] {
                let (mut stream, _) = listener.accept().unwrap();
                read_request(&mut stream);
                send(&mut stream, &reply(provider, tool));
            }
        });
        let output = command(work.path())
            .arg("--telemetry-output")
            .arg(work.path().join("metrics.jsonl"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        server.join().unwrap();
        let value = envelope(&output);
        let t = &value["telemetry"];
        assert_eq!(t["version"], 2);
        assert_eq!(t["reservation"], "fresh");
        assert_eq!(t["model_calls"], 2);
        assert_eq!(t["round_count"], 2);
        assert_eq!(t["tool_calls"], value["tool_calls"]);
        assert_eq!(t["total_input_tokens"], 200);
        assert_eq!(t["total_output_tokens"], 10);
        assert_eq!(t["total_tokens"], 210);
        assert_eq!(t["cache_hit_ratio"], 0.5);
        for (index, call) in t["rounds"].as_array().unwrap().iter().enumerate() {
            assert_eq!(call["round"], index + 1);
            assert_eq!(call["attempt"], 1);
            assert_eq!(call["input_tokens"], 100);
            assert_eq!(call["uncached_input_tokens"], 50);
            assert!(call["request_estimate_bytes"].as_u64().unwrap() > 8192);
            assert!(call["ttft_ms"].as_u64().unwrap() >= 20);
            assert!(call["wall_ms"].as_u64().unwrap() >= call["ttft_ms"].as_u64().unwrap());
            assert!(call["gen_ms"].is_null());
            assert!(call["tok_s"].is_null());
            if provider != "anthropic" {
                assert_eq!(call["reasoning_tokens"], 2);
            }
        }
        assert_eq!(t["tools"][0]["name"], "read_file");
        assert_eq!(t["tools"][0]["exit_class"], "ok");
        assert!(t["tools"][0]["output_bytes"].as_u64().unwrap() > 0);
        assert_eq!(persisted(work.path()), *t);
        let events: Vec<Value> = std::fs::read_to_string(work.path().join("metrics.jsonl"))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(events.len(), 3);
        assert_eq!(events[2]["telemetry"], *t);
        let replay = command(work.path()).args(["--turn", "1"]).output().unwrap();
        assert!(replay.status.success());
        let replay = envelope(&replay);
        assert_eq!(replay["telemetry"]["reservation"], "replay");
        assert_eq!(replay["telemetry"]["model_calls"], 0);
        assert!(replay["telemetry"]["rounds"].as_array().unwrap().is_empty());
        assert_eq!(persisted(work.path()), *t);
    }
}

#[test]
fn retry_attempts_keep_class_status_hint_actual_wait_and_success() {
    let work = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    profile(work.path(), listener.local_addr().unwrap(), "openai");
    let server = loopback::spawn(listener, move |listener| {
        let (mut stream, _) = listener.accept().unwrap();
        let first = read_request(&mut stream);
        write!(stream,"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 0\r\nContent-Length: 10\r\nConnection: close\r\n\r\nrate limit").unwrap();
        drop(stream);
        let (mut stream, _) = listener.accept().unwrap();
        assert_eq!(read_request(&mut stream), first);
        send(&mut stream, &reply("openai", false));
    });
    let output = command(work.path()).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    server.join().unwrap();
    let value = envelope(&output);
    let t = &value["telemetry"];
    assert_eq!(t["model_calls"], 2);
    assert_eq!(t["rounds"][1]["attempt"], 2);
    let retry = &t["rounds"][0]["retry"];
    assert_eq!(retry["error_class"], "rate-limit");
    assert_eq!(retry["http_status"], 429);
    assert_eq!(retry["retry_after_ms"], 0);
    assert!(retry["backoff_ms"].as_u64().unwrap() >= 250);
    assert_eq!(retry["succeeded"], true);
    assert!(t["total_input_tokens"].is_null()); // the throttled attempt supplied no usage
    assert_eq!(persisted(work.path()), *t);
}

#[test]
fn timeout_failure_carries_wall_time_and_durable_cancelled_call() {
    let work = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    profile(work.path(), listener.local_addr().unwrap(), "openai");
    let server = loopback::spawn(listener, move |listener| {
        let (mut stream, _) = listener.accept().unwrap();
        read_request(&mut stream);
        std::thread::sleep(Duration::from_millis(1400));
    });
    let output = command(work.path())
        .args(["--turn-time", "1s"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(6),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    server.join().unwrap();
    let value = envelope(&output);
    assert_eq!(value["error"]["code"], "turn-time-exhausted");
    let t = &value["telemetry"];
    assert!(t["turn_wall_ms"].as_u64().unwrap() >= 900);
    assert_eq!(t["rounds"][0]["outcome"], "cancelled");
    assert!(t["rounds"][0]["ttft_ms"].is_null());
    assert!(t["total_tokens"].is_null());
    assert_eq!(persisted(work.path()), *t);
}

#[test]
fn telemetry_file_is_create_only_and_cannot_target_the_session_store() {
    let work = tempfile::tempdir().unwrap();
    std::fs::write(work.path().join("existing"), "preserve").unwrap();
    let output = command(work.path())
        .arg("--telemetry-output")
        .arg(work.path().join("existing"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(
        std::fs::read_to_string(work.path().join("existing")).unwrap(),
        "preserve"
    );
    assert_eq!(envelope(&output)["error"]["code"], "telemetry-output");
    let store = llxprt_code_rs::session::SessionStore::load_at(
        &llxprt_code_rs::session::SessionId::parse("telemetry").unwrap(),
        work.path(),
    )
    .unwrap();
    let output = command(work.path())
        .arg("--telemetry-output")
        .arg(store.session_dir().join("metrics.jsonl"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert!(!store.session_dir().join("metrics.jsonl").exists());
}

#[test]
fn missing_and_inconsistent_provider_counters_remain_unknown_and_finite() {
    for mode in ["missing", "inconsistent", "zero"] {
        let work = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        profile(work.path(), listener.local_addr().unwrap(), "openai");
        let server = loopback::spawn(listener, move |listener| {
            let (mut stream, _) = listener.accept().unwrap();
            read_request(&mut stream);
            let mut response = reply("openai", false);
            match mode {
                "missing" => {
                    response.as_object_mut().unwrap().remove("usage");
                }
                "inconsistent" => {
                    response["usage"]["prompt_tokens_details"]["cached_tokens"] = json!(101)
                }
                _ => {
                    response["usage"] = json!({"prompt_tokens":0,"completion_tokens":0,"total_tokens":0,"prompt_tokens_details":{"cached_tokens":0}})
                }
            }
            send(&mut stream, &response);
        });
        let output = command(work.path()).output().unwrap();
        assert!(output.status.success());
        server.join().unwrap();
        let value = envelope(&output);
        let t = &value["telemetry"];
        assert!(t["cache_hit_ratio"].is_null());
        assert!(t["rounds"][0]["tok_s"].is_null());
        if mode == "missing" {
            assert!(t["total_output_tokens"].is_null());
        }
        if mode == "inconsistent" {
            assert!(t["rounds"][0]["uncached_input_tokens"].is_null());
        }
        if mode == "zero" {
            assert_eq!(t["total_tokens"], 0);
        }
        assert_eq!(persisted(work.path()), *t);
    }
}

#[test]
fn tool_refusal_and_truncation_are_counted_without_executing_refused_calls() {
    for refused in [true, false] {
        let work = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        profile(work.path(), listener.local_addr().unwrap(), "openai");
        std::fs::write(work.path().join("input"), "x".repeat(4096)).unwrap();
        let server = loopback::spawn(listener, move |listener| {
            for tool in [true, false] {
                let (mut stream, _) = listener.accept().unwrap();
                read_request(&mut stream);
                let mut response = reply("openai", tool);
                if tool {
                    response["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"] =
                        json!("{\"path\":\"input\",\"max_output_bytes\":256}");
                    if refused {
                        let extra = response["choices"][0]["message"]["tool_calls"][0].clone();
                        response["choices"][0]["message"]["tool_calls"]
                            .as_array_mut()
                            .unwrap()
                            .push(extra);
                        response["choices"][0]["message"]["tool_calls"][1]["id"] = json!("refused");
                    }
                }
                send(&mut stream, &response);
            }
        });
        let output = command(work.path())
            .args(["--max-tool-calls", "1"])
            .output()
            .unwrap();
        server.join().unwrap();
        let value = envelope(&output);
        let t = &value["telemetry"];
        assert_eq!(t["tool_calls"], 1);
        assert_eq!(t["refusal_events"], usize::from(refused));
        assert_eq!(t["tools"][0]["truncated"], true);
        assert!(t["tools"][0]["output_bytes"].as_u64().unwrap() <= 256);
        if refused {
            assert_eq!(t["tools"][1]["exit_class"], "budget-refused");
        }
        assert_eq!(persisted(work.path()), *t);
    }
}

#[test]
fn partial_tail_recovery_is_visible_and_store_reads_are_not_old_prefix_estimates() {
    let work = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    profile(work.path(), listener.local_addr().unwrap(), "openai");
    let server = loopback::spawn(listener, move |listener| {
        let (mut stream, _) = listener.accept().unwrap();
        read_request(&mut stream);
        send(&mut stream, &reply("openai", false));
    });
    let output = command(work.path()).output().unwrap();
    assert!(output.status.success());
    server.join().unwrap();
    let value = envelope(&output);
    let store = llxprt_code_rs::session::SessionStore::load_at(
        &llxprt_code_rs::session::SessionId::parse("telemetry").unwrap(),
        work.path(),
    )
    .unwrap();
    let manifest: Value = serde_json::from_slice(
        &std::fs::read(store.session_dir().join("session.manifest.json")).unwrap(),
    )
    .unwrap();
    let snapshot_bytes = std::fs::metadata(
        store
            .session_dir()
            .join(manifest["current"]["snapshot"].as_str().unwrap()),
    )
    .unwrap()
    .len();
    let segment = store
        .session_dir()
        .join(manifest["current"]["segment"].as_str().unwrap());
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&segment)
        .unwrap();
    file.write_all(b"LLX").unwrap();
    let tail_bytes = file.metadata().unwrap().len();
    drop(file);
    drop(store);
    let output = command(work.path()).args(["--turn", "1"]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let replay = envelope(&output);
    assert_eq!(replay["telemetry"]["repaired_tail"], true);
    let operations = replay["telemetry"]["store_operations"].as_array().unwrap();
    assert!(operations
        .iter()
        .any(|o| o["segment_read_bytes"].as_u64().unwrap() == tail_bytes));
    assert!(operations
        .iter()
        .any(|o| o["snapshot_read_bytes"].as_u64().unwrap() == snapshot_bytes));
    assert!(operations.iter().all(|o| o["segment_written_bytes"] == 0));
    assert_eq!(persisted(work.path()), value["telemetry"]);
}
