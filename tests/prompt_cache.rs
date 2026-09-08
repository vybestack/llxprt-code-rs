//! Exact HTTP-byte prefix invariants through the compiled process-per-turn CLI.
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::time::{Duration, Instant};

fn field<'a>(body: &'a str, key: &str) -> &'a str {
    let start = body.find(&format!("\"{key}\":")).unwrap() + key.len() + 3;
    let rest = &body[start..];
    let mut stream = serde_json::Deserializer::from_str(rest).into_iter::<Value>();
    stream.next().unwrap().unwrap();
    &rest[..stream.byte_offset()]
}

fn response(provider: &str, round: usize) -> Value {
    let tool = round % 2 == 0;
    match provider {
        "anthropic" => {
            json!({"id":"msg", "type":"message", "role":"assistant", "model":"fixture", "stop_reason": if tool {"tool_use"} else {"end_turn"},
            "content": if tool {json!([{"type":"tool_use","id":"call","name":"read_file","input":{"path":"fixture.txt"}}])} else {json!([{"type":"text","text":"done"}])},
            "usage":{"input_tokens":20,"output_tokens":5,"cache_creation_input_tokens":30,"cache_read_input_tokens":50}})
        }
        "openai-responses" => {
            json!({"id":"resp", "object":"response", "created_at":1, "model":"fixture", "status":"completed",
            "output": if tool {json!([{"id":"item","type":"function_call","status":"completed","call_id":"call","name":"read_file","arguments":"{\"path\":\"fixture.txt\"}"}])} else {json!([{"id":"item2","type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":"done"}]}])},
            "usage":{"input_tokens":100,"output_tokens":5,"total_tokens":105,"input_tokens_details":{"cached_tokens":50}}})
        }
        _ => {
            json!({"id":"chat", "object":"chat.completion", "created":1, "model":"fixture", "choices":[{"index":0,"finish_reason":if tool {"tool_calls"} else {"stop"}, "message": if tool {json!({"role":"assistant","content":null,"tool_calls":[{"id":"call","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"fixture.txt\"}"}}]})} else {json!({"role":"assistant","content":"done"})}}],
            "usage":{"prompt_tokens":100,"completion_tokens":5,"total_tokens":105,"prompt_tokens_details":{"cached_tokens":50}}})
        }
    }
}

#[test]
fn exact_wire_round_and_turn_prefixes_and_cache_accounting() {
    for provider in ["openai", "openai-responses", "anthropic"] {
        for off in [false, true] {
            let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
            let work = tempfile::tempdir_in(root.join("tmp")).unwrap();
            std::fs::create_dir(work.path().join("profiles")).unwrap();
            std::fs::write(work.path().join("fixture.txt"), "stable evidence").unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let addr = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let mut bodies = Vec::new();
                for round in 0..4 {
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
                    let mut body = vec![0; length.unwrap()];
                    reader.read_exact(&mut body).unwrap();
                    bodies.push(String::from_utf8(body).unwrap());
                    let reply = response(provider, round)
                        .to_string()
                        .replace("\"call\"", &format!("\"call-{}\"", round / 2));
                    write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", reply.len(), reply).unwrap();
                }
                bodies
            });
            let endpoint = if provider == "openai-responses" {
                format!("http://{addr}/v1/responses")
            } else {
                format!("http://{addr}/v1")
            };
            let mut ephemeral = json!({"base-url":endpoint,"auth-key":"fixture-key"});
            if off {
                ephemeral["prompt-caching"] = json!("off");
            }
            std::fs::write(
                work.path().join("profiles/cache.json"),
                json!({"provider":provider,"model":"fixture","ephemeralSettings":ephemeral,
                    "modelParams": if provider == "openai" { json!({"prompt_cache_key":"unowned-override"}) } else { json!({}) }})
                    .to_string(),
            )
            .unwrap();
            for turn in 0..2 {
                let output = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
                    .env("LLXPRT_CONFIG_HOME", work.path())
                    .args(["--profile", "cache", "--session", "cache-session", "--cwd"])
                    .arg(work.path())
                    .args([
                        "-p",
                        &format!("Read fixture.txt then say done, turn {turn}"),
                    ])
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{provider} off={off} turn={turn}: {}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                let stderr = String::from_utf8(output.stderr).unwrap();
                let aggregate: Value = stderr
                    .lines()
                    .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                    .filter(|event| event["event"] == "prompt_cache_run")
                    .next_back()
                    .unwrap();
                assert_eq!(aggregate["usage"]["hit_ratio"], 0.5);
                assert_eq!(aggregate["usage"]["measured_calls"], 2);
                let events: Vec<Value> = stderr
                    .lines()
                    .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                    .filter(|event| event["event"] == "prompt_cache_call")
                    .collect();
                assert_eq!(events.len(), 2);
                for event in events {
                    assert_eq!(event["total_input_tokens"], 100);
                    assert_eq!(event["cached_input_tokens"], 50);
                    assert_eq!(event["uncached_input_tokens"], 50);
                }
            }
            let bodies = server.join().unwrap();
            let tools = field(&bodies[0], "tools");
            for body in &bodies[1..] {
                assert_eq!(field(body, "tools").as_bytes(), tools.as_bytes());
            }
            let parsed_tools: Value = serde_json::from_str(tools).unwrap();
            let names: Vec<&str> = parsed_tools
                .as_array()
                .unwrap()
                .iter()
                .map(|tool| {
                    tool["name"]
                        .as_str()
                        .or_else(|| tool["function"]["name"].as_str())
                        .unwrap()
                })
                .collect();
            assert_eq!(
                names,
                [
                    "read_file",
                    "write_file",
                    "replace",
                    "list_directory",
                    "search_file_content"
                ]
            );
            if provider == "anthropic" {
                let system = field(&bodies[0], "system");
                for body in &bodies {
                    assert_eq!(field(body, "system"), system);
                }
                assert_eq!(system.contains("cache_control"), !off);
                assert_eq!(tools.contains("cache_control"), !off);
                let changed = system.replacen("You", "Timestamp: 1. You", 1);
                assert_ne!(system.as_bytes(), changed.as_bytes(), "negative control");
            } else {
                for body in &bodies {
                    let value: Value = serde_json::from_str(body).unwrap();
                    if off {
                        assert!(value.get("prompt_cache_key").is_none());
                    } else {
                        assert_eq!(value["prompt_cache_key"], "cache-session");
                    }
                }
            }
            let messages = if provider == "openai-responses" {
                "input"
            } else {
                "messages"
            };
            for pair in [&bodies[0..2], &bodies[2..4]] {
                let first = field(&pair[0], messages);
                let next = field(&pair[1], messages);
                assert!(
                    next.as_bytes()
                        .starts_with(&first.as_bytes()[..first.len() - 1]),
                    "round prefix drift: {provider}"
                );
            }
            // History representation can change across turns; the system head must not.
            if provider == "openai-responses" {
                let instructions = field(&bodies[0], "instructions");
                for body in &bodies {
                    assert_eq!(field(body, "instructions"), instructions);
                }
                assert_ne!(
                    instructions,
                    instructions.replacen("You", "Timestamp: 1. You", 1)
                );
            }
            if provider == "openai" {
                let first = &field(&bodies[0], messages)[1..];
                let mut stream = serde_json::Deserializer::from_str(first).into_iter::<Value>();
                stream.next().unwrap().unwrap();
                let system = &first[..stream.byte_offset()];
                for body in &bodies {
                    let raw = field(body, messages);
                    assert!(raw[1..].starts_with(system), "turn head drift: {provider}");
                }
                assert!(!field(&bodies[0], messages)[1..]
                    .starts_with(&system.replace("You", "Timestamp: 1. You")));
            }
            let mut reordered = parsed_tools.as_array().unwrap().clone();
            reordered.swap(0, 1);
            assert_ne!(
                tools.as_bytes(),
                serde_json::to_vec(&reordered).unwrap(),
                "tool-order negative control"
            );
        }
    }
}
