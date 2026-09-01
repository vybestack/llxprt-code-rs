use serde_json::Value;
use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
}

fn events(path: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn config_failure_still_finalizes_a_well_ordered_profile() {
    let temp = tempfile::tempdir().unwrap();
    let profile = temp.path().join("memory.jsonl");
    let output = bin()
        .env("LLXPRT_CONFIG_HOME", temp.path())
        .arg("--profile")
        .arg("missing")
        .arg("--session")
        .arg("mem-config")
        .arg("--mem-profile")
        .arg(&profile)
        .arg("-p")
        .arg("hello")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(
        output.stdout.iter().filter(|byte| **byte == b'\n').count(),
        1
    );
    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(parsed["error"]["code"], "profile-missing");
    let events = events(&profile);
    assert_eq!(events.first().unwrap()["phase"], "startup_observed");
    assert_eq!(events.last().unwrap()["phase"], "profile_complete");
    assert_eq!(events.last().unwrap()["outcome"], "config");
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event["seq"], (index + 1) as u64);
        assert!(event["rss_bytes"].as_u64().unwrap() > 0);
    }
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .starts_with("mem-profile:"));
}

#[test]
fn collision_is_exit_seven_before_session_side_effects() {
    let temp = tempfile::tempdir().unwrap();
    let profile = temp.path().join("exists.jsonl");
    std::fs::write(&profile, "owned").unwrap();
    let output = bin()
        .env("LLXPRT_CONFIG_HOME", temp.path())
        .arg("--session")
        .arg("mem-collision")
        .arg("--mem-profile")
        .arg(&profile)
        .arg("-p")
        .arg("hello")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7));
    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(parsed["error"]["code"], "mem-profile");
    assert_eq!(parsed["error"]["stage"], "sink_init");
    assert_eq!(parsed["error"]["session_status"], "ok");
    assert_eq!(std::fs::read_to_string(&profile).unwrap(), "owned");
    assert!(!temp.path().join("code-rs-sessions").exists());
}

#[test]
fn help_documents_flag_without_creating_it() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("help.jsonl");
    let output = bin()
        .arg("--help")
        .arg("--mem-profile")
        .arg(&path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("--mem-profile <PATH>"));
    assert!(!path.exists());
}

#[test]
fn loopback_success_profiles_paired_model_call_and_counter_agreement() {
    use std::io::{BufRead as _, Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
        let mut length = 0usize;
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
        let mut request = vec![0; length];
        reader.read_exact(&mut request).unwrap();
        let body = r#"{"id":"1","object":"chat.completion","created":1,"model":"loopback","choices":[{"index":0,"message":{"role":"assistant","content":"done"},"finish_reason":"stop"}]}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    });
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("profiles")).unwrap();
    std::fs::write(
        temp.path().join("profiles/memory.json"),
        serde_json::json!({
            "provider": "openai",
            "model": "loopback",
            "ephemeralSettings": {
                "base-url": format!("http://{address}"),
                "auth-key": "test-key"
            }
        })
        .to_string(),
    )
    .unwrap();
    let profile = temp.path().join("memory.jsonl");
    let output = bin()
        .env("LLXPRT_CONFIG_HOME", temp.path())
        .arg("--profile")
        .arg("memory")
        .arg("--session")
        .arg("mem-success")
        .arg("--cwd")
        .arg(temp.path())
        .arg("--mem-profile")
        .arg(&profile)
        .arg("-p")
        .arg("hello")
        .output()
        .unwrap();
    server.join().unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    let events = events(&profile);
    let phases: Vec<_> = events
        .iter()
        .map(|event| event["phase"].as_str().unwrap())
        .collect();
    assert!(phases
        .windows(2)
        .any(|pair| pair == ["model_call_before", "model_call_after"]));
    assert_eq!(
        events.last().unwrap()["executed_tool_calls"],
        envelope["tool_calls"]
    );
    assert_eq!(events.last().unwrap()["outcome"], "ok");
    for event in &events {
        assert!(event.get("request_estimate_bytes").is_some());
        assert!(event.get("session_slot_input_bytes").is_some());
        assert!(event["peak_rss_bytes"].as_u64().unwrap() > 0);
    }
}
