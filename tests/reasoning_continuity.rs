//! Stock process-per-turn provider contracts: forced summary, restart and retained recovery.
#[path = "../src/test_loopback.rs"]
mod loopback;
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::path::Path;
use std::process::Command;

fn reply(provider: &str, index: usize) -> Value {
    let tool = matches!(index, 0 | 2 | 3);
    let id = format!("call-{index}");
    match provider {
        "anthropic" => {
            json!({"id":"response","type":"message","role":"assistant","model":"fixture",
            "stop_reason":if tool {"tool_use"} else {"end_turn"}, "content": [
                {"type":"thinking","thinking":format!("readable-{index}"),"signature":format!("signature-private-{index}")},
                {"type":"redacted_thinking","data":format!("redacted-private-{index}")},
                if tool {json!({"type":"tool_use","id":id,"name":"read_file","input":{"path":"input.txt"}})}
                else {json!({"type":"text","text":"done"})}
            ],"usage":{"input_tokens":10,"output_tokens":10}})
        }
        "openai-responses" => {
            json!({"id":"response","object":"response","created_at":1,"model":"fixture","status":"completed",
            "output":[{"type":"reasoning","id":format!("reason-{index}"),"summary":if index==0 {json!([])} else {json!([{"type":"summary_text","text":format!("readable-{index}"),"annotations":[{"type":"fixture_annotation","value":"retained"}]}])},"encrypted_content":format!("encrypted-private-{index}"),"status":"completed"},
                if tool {json!({"type":"function_call","id":format!("item-{index}"),"call_id":id,"name":"read_file","arguments":"{\"path\":\"input.txt\"}","status":"completed"})}
                else {json!({"type":"message","id":format!("msg-{index}"),"role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"done"}],"status":"completed"})}
            ]})
        }
        _ => {
            json!({"id":"response","object":"chat.completion","created":1,"model":"fixture","choices":[{"index":0,"finish_reason":if tool {"tool_calls"} else {"stop"},"message":{
                "role":"assistant","content":if tool {""} else {"done"},"reasoning_content":format!("readable-{index}"),
                "tool_calls":if tool {json!([{"id":id,"type":"function","function":{"name":"read_file","arguments":"{\"path\":\"input.txt\"}"}}])} else {json!(null)}
            }}]})
        }
    }
}

fn accept(listener: &loopback::Listener) -> loopback::Connection {
    listener.accept().unwrap().0
}

fn body(socket: &mut loopback::Connection) -> Value {
    let mut reader = std::io::BufReader::new(socket);
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
    serde_json::from_slice(&bytes).unwrap()
}

fn cli(root: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"));
    cmd.env("LLXPRT_CONFIG_HOME", root)
        .env("NO_PROXY", "127.0.0.1,localhost");
    cmd
}

fn files(root: &Path) -> Vec<(std::path::PathBuf, Vec<u8>, u32, std::time::SystemTime)> {
    use std::os::unix::fs::PermissionsExt;
    let mut out = Vec::new();
    for e in std::fs::read_dir(root).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(files(&p));
        } else {
            let m = std::fs::metadata(&p).unwrap();
            out.push((
                p.clone(),
                std::fs::read(p).unwrap(),
                m.permissions().mode(),
                m.modified().unwrap(),
            ));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
fn provider_state_survives_forced_summary_restart_multiple_rounds_and_retained_history() {
    for provider in ["openai-responses", "anthropic", "openai"] {
        run(provider);
    }
}

fn run(provider: &'static str) {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("profiles")).unwrap();
    std::fs::write(root.path().join("input.txt"), "admitted evidence").unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();

    let addr = listener.local_addr().unwrap();
    std::fs::write(root.path().join("profiles/fixture.json"), json!({"provider":provider,"model":"fixture", "ephemeralSettings":{"base-url":format!("http://{addr}/v1"),"auth-key":"fixture-configured-key"}}).to_string()).unwrap();
    let server = loopback::spawn(listener, move |listener| {
        let mut requests = Vec::new();
        for i in 0..5 {
            let mut socket = accept(&listener);
            requests.push(body(&mut socket));
            let response = reply(provider, i).to_string();
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
        }
        requests
    });
    for turn in 0..2 {
        let output = cli(root.path())
            .args(["--profile", "fixture", "--session", "reasoning", "--cwd"])
            .arg(root.path())
            .args([
                "--max-tool-calls",
                if turn == 0 { "1" } else { "2" },
                "-p",
                if turn == 0 { "first" } else { "second" },
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{provider}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(envelope["status"], "ok");
        assert_eq!(envelope["summary"], "done");
        assert_eq!(envelope["tool_calls"], if turn == 0 { 1 } else { 2 });
        let stderr = String::from_utf8(output.stderr).unwrap();
        for opaque in ["signature-private", "redacted-private", "encrypted-private"] {
            assert!(!stderr.contains(opaque));
        }
        let events: Vec<Value> = stderr
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(
            events.len(),
            if turn == 0 && provider == "openai-responses" {
                1
            } else if turn == 0 {
                2
            } else {
                3
            }
        );
        assert!(events.iter().all(|e| e["type"] == "thinking"));
        assert!(events.iter().all(|e| e["turn"] == turn + 1));
        assert_eq!(
            events.last().unwrap()["round"],
            if turn == 0 { 2 } else { 3 }
        );
        assert_lookback(root.path(), turn);
        if turn == 0 {
            retain(root.path());
        }
    }
    let requests = server.join().unwrap();
    for (i, request) in requests.iter().enumerate() {
        if provider == "openai-responses" {
            assert_eq!(request["store"], false);
            assert!(request.get("previous_response_id").is_none());
            assert_eq!(request["reasoning"]["summary"], "auto");
            assert_eq!(request["include"], json!(["reasoning.encrypted_content"]));
        }
        for prior in 0..i {
            assert_prior(provider, request, prior);
        }
    }
    // First turn's final-answer phase survives the process restart.
    if provider == "openai-responses" {
        assert!(requests[2]["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["phase"] == "final_answer"));
    }
}

fn assert_prior(provider: &str, request: &Value, prior: usize) {
    let needle = format!("readable-{prior}");
    if provider == "openai-responses" {
        let input = request["input"].as_array().unwrap();
        let pos = input
            .iter()
            .position(|v| v["id"] == format!("reason-{prior}"))
            .unwrap();
        assert_eq!(
            input[pos]["encrypted_content"],
            format!("encrypted-private-{prior}")
        );
        assert_eq!(
            input[pos]["summary"],
            reply(provider, prior)["output"][0]["summary"]
        );
        if matches!(prior, 0 | 2 | 3) {
            assert_eq!(input[pos + 1]["type"], "function_call");
            assert_eq!(input[pos + 1]["call_id"], format!("call-{prior}"));
            assert_eq!(input[pos + 2]["type"], "function_call_output");
            assert_eq!(input[pos + 2]["call_id"], format!("call-{prior}"));
        }
    } else if provider == "anthropic" {
        let message = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "assistant" && m["content"][0]["thinking"] == needle)
            .unwrap();
        assert_eq!(
            message["content"][0]["signature"],
            format!("signature-private-{prior}")
        );
        assert_eq!(message["content"][1]["type"], "redacted_thinking");
        assert_eq!(
            message["content"][1]["data"],
            format!("redacted-private-{prior}")
        );
        if matches!(prior, 0 | 2 | 3) {
            assert_eq!(message["content"][2]["id"], format!("call-{prior}"));
        }
    } else {
        assert!(request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "assistant" && m["reasoning_content"] == needle));
    }
}

fn assert_lookback(root: &Path, turn: usize) {
    let dir = root.join("code-rs-sessions/reasoning");
    let before = files(&dir);
    for json in [true, false] {
        let mut cmd = cli(root);
        cmd.args(["transcript", "--session", "reasoning", "--max-bytes", "8"]);
        if json {
            cmd.arg("--json");
        }
        let out = cmd.output().unwrap();
        assert!(out.status.success());
        assert!(out.stderr.is_empty());
        let text = String::from_utf8(out.stdout).unwrap();
        assert!(!text.contains("private"));
        if json {
            let transcript: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(
                transcript["turns"][turn]["rounds"]
                    .as_array()
                    .unwrap()
                    .len(),
                if turn == 0 { 2 } else { 3 }
            );
            assert_eq!(
                transcript["turns"][turn]["rounds"]
                    .as_array()
                    .unwrap()
                    .last()
                    .unwrap()["thinking"],
                format!("readable-{}", if turn == 0 { 1 } else { 4 })
            );
        } else {
            assert!(text.contains("Thinking:"));
        }
        assert_eq!(before, files(&dir));
    }
}

fn retain(root: &Path) {
    let id = llxprt_code_rs::session::SessionId::parse("reasoning").unwrap();
    let store = llxprt_code_rs::session::SessionStore::load_at(&id, root).unwrap();
    store.replace_snapshot(&store.snapshot().unwrap()).unwrap();
    let manifest: Value = serde_json::from_slice(
        &std::fs::read(store.session_dir().join("session.manifest.json")).unwrap(),
    )
    .unwrap();
    std::fs::write(
        store
            .session_dir()
            .join(manifest["current"]["snapshot"].as_str().unwrap()),
        "corrupt active snapshot",
    )
    .unwrap();
    assert_lookback(root, 0);
}

#[test]
fn changed_model_refuses_private_history_before_any_transport_request() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("profiles")).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();

    let addr = listener.local_addr().unwrap();
    let profile = root.path().join("profiles/fixture.json");
    let config = |model| {
        json!({"provider":"openai","model":model,"ephemeralSettings":{"base-url":format!("http://{addr}/v1"),"auth-key":"key"}}).to_string()
    };
    std::fs::write(&profile, config("first-model")).unwrap();
    let server = loopback::spawn(listener, move |listener| {
        let mut socket = accept(&listener);
        let _ = body(&mut socket);
        let response = reply("openai", 1).to_string();
        write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).unwrap();
        // Return the still-listening socket to check that restart sent nothing.
        listener
    });
    let output = cli(root.path())
        .args(["--profile", "fixture", "--session", "switch", "-p", "first"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let listener = server.join().unwrap();
    std::fs::write(profile, config("second-model")).unwrap();
    let output = cli(root.path())
        .args([
            "--profile",
            "fixture",
            "--session",
            "switch",
            "-p",
            "second",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(5));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(envelope["error"]["message"]
        .as_str()
        .unwrap()
        .contains("route/model mismatch"));
    assert!(output.stderr.is_empty());
    assert!(matches!(listener.accept(),Err(e) if e.kind()==std::io::ErrorKind::WouldBlock));
}
