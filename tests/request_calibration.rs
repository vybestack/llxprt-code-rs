//! Stock CLI exact-wire measurement, normalized usage, restart and contract invalidation.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{BufRead, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output};

fn request(stream: &mut TcpStream) -> Vec<u8> {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
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
    bytes
}
fn send(stream: &mut TcpStream, body: &Value) {
    let body = body.to_string();
    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
}
fn profile(
    root: &Path,
    address: std::net::SocketAddr,
    model: &str,
    provider: &str,
    limit: Option<u64>,
) {
    std::fs::create_dir_all(root.join("profiles")).unwrap();
    let mut settings = json!({"base-url":format!("http://{address}/v1"), "maxOutputTokens":100});
    if let Some(limit) = limit {
        settings["context-limit"] = json!(limit);
    }
    std::fs::write(
        root.join("profiles/calibrate.json"),
        json!({"provider":provider,"model":model,"ephemeralSettings":settings}).to_string(),
    )
    .unwrap();
}
fn run(root: &Path, prompt: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
        .env("LLXPRT_CONFIG_HOME", root)
        .env("NO_PROXY", "127.0.0.1,localhost")
        .args([
            "--session",
            "calibration",
            "--profile",
            "calibrate",
            "--cwd",
        ])
        .arg(root)
        .args(["-p", prompt])
        .output()
        .unwrap()
}
fn reply(provider: &str, input: u64) -> Value {
    if provider == "anthropic" {
        json!({"id":"msg","type":"message","role":"assistant","model":"fixture","stop_reason":"end_turn",
            "content":[{"type":"text","text":"done"}],"usage":{"input_tokens":input,"output_tokens":5,
                "cache_creation_input_tokens":30,"cache_read_input_tokens":50}})
    } else {
        json!({"id":"chat","object":"chat.completion","created":1,"model":"fixture",
            "choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"done"}}],
            "usage":{"prompt_tokens":input,"completion_tokens":5,"total_tokens":input+5,
                "prompt_tokens_details":{"cached_tokens":50},"completion_tokens_details":{"reasoning_tokens":2}}})
    }
}
fn bound(output: &Output) -> Value {
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(output.status.success(), "{envelope}");
    assert!(output.stderr.is_empty());
    envelope["telemetry"]["rounds"][0]["request_bound"].clone()
}
#[test]
fn exact_wire_usage_and_restart_margin_are_bound_to_route_model() {
    for provider in ["openai", "anthropic"] {
        let work = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        profile(
            work.path(),
            listener.local_addr().unwrap(),
            "unregistered-fixture",
            provider,
            None,
        );
        let server = std::thread::spawn(move || {
            let mut wires = vec![];
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let wire = request(&mut stream);
                wires.push(wire);
                send(&mut stream, &reply(provider, 10000));
            }
            wires
        });
        let first = bound(&run(work.path(), "first multilingual 中文"));
        let second = bound(&run(work.path(), "second code fn main() {}"));
        let address: Value = serde_json::from_slice(
            &std::fs::read(work.path().join("profiles/calibrate.json")).unwrap(),
        )
        .unwrap();
        let mut switched = address;
        switched["model"] = json!("other-fixture");
        std::fs::write(
            work.path().join("profiles/calibrate.json"),
            switched.to_string(),
        )
        .unwrap();
        let third = bound(&run(work.path(), "third schema {}"));
        let wires = server.join().unwrap();
        for (b, wire) in [&first, &second, &third].into_iter().zip(wires) {
            assert_eq!(b["wire_digest"], format!("{:x}", Sha256::digest(&wire)));
            assert_eq!(b["wire_bytes"], wire.len() as u64);
            assert_eq!(b["contract"]["tokenizer"], Value::Null);
            assert_eq!(b["capability"], "tokenizer_unavailable_byte_heuristic");
            assert_eq!(b["terms"]["output_reserve"], 100);
            assert_eq!(
                b["observed_input"],
                if provider == "anthropic" {
                    10080
                } else {
                    10000
                }
            );
        }
        assert!(first["margin"].is_null());
        let residual =
            first["observed_input"].as_u64().unwrap() - first["estimated_input"].as_u64().unwrap();
        assert_eq!(second["margin"], residual);
        assert_eq!(
            second["bounded_input"],
            second["estimated_input"].as_u64().unwrap() + residual
        );
        assert!(third["margin"].is_null());
        assert_ne!(third["contract"], second["contract"]);
        let state = llxprt_code_rs::session::SessionStore::read_transcript_at(
            &llxprt_code_rs::session::SessionId::parse("calibration").unwrap(),
            work.path(),
        )
        .unwrap();
        let persisted =
            serde_json::to_value(state.branches[1].telemetry.as_ref().unwrap()).unwrap();
        assert_eq!(persisted["rounds"][0]["request_bound"], second);
    }
}

#[test]
fn calibrated_admission_refuses_before_send_after_restart() {
    let work = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    profile(work.path(), address, "fixture", "openai", Some(30000));
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        request(&mut stream);
        send(&mut stream, &reply("openai", 50000));
        listener.set_nonblocking(true).unwrap();
        listener
    });
    let first = bound(&run(work.path(), "first"));
    assert!(first["margin"].is_null());
    let listener = server.join().unwrap();
    let refused = run(work.path(), "second unchanged mandatory prompt");
    assert_eq!(refused.status.code(), Some(5));
    let envelope: Value = serde_json::from_slice(&refused.stdout).unwrap();
    assert_eq!(envelope["status"], "error");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("calibrated")
            || envelope.to_string().contains("calibrated")
    );
    assert!(
        listener.accept().is_err(),
        "calibrated refusal must not touch send"
    );
    let measured = &envelope["telemetry"]["rounds"][0]["request_bound"];
    assert!(measured["bounded_input"].as_u64().unwrap() > 30000);
    assert!(measured["observed_input"].is_null());
}

#[test]
fn observed_margin_failure_is_typed_not_silently_accepted() {
    let work = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    profile(
        work.path(),
        listener.local_addr().unwrap(),
        "fixture",
        "openai",
        None,
    );
    let server = std::thread::spawn(move || {
        for input in [10000, 50000] {
            let (mut stream, _) = listener.accept().unwrap();
            request(&mut stream);
            send(&mut stream, &reply("openai", input));
        }
    });
    bound(&run(work.path(), "train"));
    let output = run(work.path(), "held out");
    server.join().unwrap();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(6));
    assert_eq!(value["status"], "error");
    assert!(value.to_string().contains("request-calibration"));
    let measured = &value["telemetry"]["rounds"][0]["request_bound"];
    assert_eq!(measured["observed_input"], 50000);
    assert!(measured["bounded_input"].as_u64().unwrap() < 50000);
}

#[test]
fn missing_tokenizer_keeps_recognized_overflow_one_shot_and_new_prompt() {
    let work = tempfile::tempdir().unwrap();
    std::fs::write(work.path().join("input"), "small").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    profile(
        work.path(),
        listener.local_addr().unwrap(),
        "fixture",
        "openai",
        None,
    );
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        request(&mut stream);
        let tool = json!({"id":"chat","object":"chat.completion","created":1,"model":"fixture",
            "choices":[{"index":0,"finish_reason":"tool_calls","message":{"role":"assistant",
                "content":"old narration ".repeat(2000),"tool_calls":[{"id":"read","type":"function",
                    "function":{"name":"read_file","arguments":"{\"path\":\"input\"}"}}]}}],
            "usage":{"prompt_tokens":100,"completion_tokens":100,"total_tokens":200}});
        send(&mut stream, &tool);
        let (mut stream, _) = listener.accept().unwrap();
        request(&mut stream);
        send(&mut stream, &reply("openai", 100));
        let (mut stream, _) = listener.accept().unwrap();
        let before = request(&mut stream);
        let error = json!({"error":{"code":"context_length_exceeded","message":"maximum context length exceeded"}}).to_string();
        write!(stream,"HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{error}",error.len()).unwrap();
        drop(stream);
        let (mut stream, _) = listener.accept().unwrap();
        let after = request(&mut stream);
        assert!(after.len() < before.len());
        for bytes in [before, after] {
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            assert!(value["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["role"] == "user" && m["content"] == "NEW mandatory 中文 unchanged"));
        }
        send(&mut stream, &reply("openai", 100));
        listener.set_nonblocking(true).unwrap();
        listener
    });
    bound(&run(work.path(), "initial read input"));
    let output = run(work.path(), "NEW mandatory 中文 unchanged");
    let listener = server.join().unwrap();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(output.status.success(), "{value}");
    assert_eq!(value["telemetry"]["model_calls"], 2);
    assert!(value["telemetry"]["rounds"][0]["request_bound"]["observed_input"].is_null());
    assert_eq!(
        value["telemetry"]["rounds"][1]["request_bound"]["capability"],
        "tokenizer_unavailable_byte_heuristic"
    );
    assert!(listener.accept().is_err());
}
