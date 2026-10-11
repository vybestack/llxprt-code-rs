//! Stock CLI request/timeout evidence; scripted replies do not prove live-model compliance.
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::time::{Duration, Instant};

fn reply(id: &str, command: Option<&str>) -> Value {
    let calls = command.map(|command| {
        vec![json!({"id":id, "type":"function", "function":{
            "name":"run_shell_command",
            "arguments":json!({"command":command,"timeout_seconds":1}).to_string()
        }})]
    });
    json!({"id":"fixture","object":"chat.completion","created":1,"model":"loopback",
        "choices":[{"index":0,"finish_reason":if calls.is_some() {"tool_calls"} else {"stop"},
            "message":{"role":"assistant","content":if calls.is_some() {""} else {"Stopped after failed creation; evidence retained."},
                "tool_calls":calls.unwrap_or_default()}}]})
}

fn serve(listener: TcpListener, replies: Vec<Value>) -> Vec<Value> {
    listener.set_nonblocking(true).unwrap();
    replies.into_iter().map(|reply| {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "CLI never connected");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("accept: {error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
        let mut length = None;
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            if line == "\r\n" { break; }
            if let Some((key, value)) = line.split_once(':') {
                if key.eq_ignore_ascii_case("content-length") {
                    length = Some(value.trim().parse::<usize>().unwrap());
                }
            }
        }
        let mut body = vec![0; length.unwrap()];
        reader.read_exact(&mut body).unwrap();
        let captured = serde_json::from_slice(&body).unwrap();
        let body = reply.to_string();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        captured
    }).collect()
}

#[test]
fn compiled_cli_sends_worktree_safety_prompt_and_retains_timeout_and_refusal() {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    std::fs::create_dir(root.path().join("profiles")).unwrap();
    std::fs::write(
        root.path().join("profiles/loopback.json"),
        json!({
            "provider":"openai","model":"loopback","ephemeralSettings":{
                "base-url":format!("http://{}",listener.local_addr().unwrap()),
                "auth-key":"loopback-fixture-only"
            }
        })
        .to_string(),
    )
    .unwrap();
    let server = std::thread::spawn(move || {
        serve(
            listener,
            vec![
                reply(
                    "create",
                    Some("mkdir incomplete; printf creation-started; sleep 3"),
                ),
                reply(
                    "guard",
                    Some("test -e incomplete/.git && touch must-not-exist"),
                ),
                reply("final", None),
            ],
        )
    });
    let output = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("TMPDIR", root.path())
        .env("LLXPRT_CONFIG_HOME", root.path())
        .args([
            "--profile",
            "loopback",
            "--session",
            "worktree-safety",
            "--allow-shell",
            "--turn-time",
            "15s",
            "--max-tool-calls",
            "4",
            "--cwd",
        ])
        .arg(root.path())
        .args([
            "-p",
            "Prepare an isolated worktree, preserving failure evidence.",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["status"], "ok");
    assert!(root.path().join("incomplete").is_dir());
    assert!(!root.path().join("must-not-exist").exists());
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 3);
    for request in &requests {
        let system = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["role"] == "system")
            .unwrap();
        let prompt = system["content"].as_str().unwrap();
        assert!(prompt.contains("confirmed terminal success"), "{prompt}");
        assert!(prompt.contains("canonical destination"), "{prompt}");
        assert!(prompt.contains("expected branch and HEAD"), "{prompt}");
        assert!(
            prompt.contains("tracked deletions and untracked files"),
            "{prompt}"
        );
    }
    let second = requests[1].to_string();
    assert!(second.contains("partial filesystem effects"), "{second}");
    assert!(second.contains("verify repository identity"), "{second}");
    assert!(second.contains("creation-started"), "{second}");
    let third = requests[2].to_string();
    assert!(third.contains("command exited with 1"), "{third}");
    assert!(third.contains("partial filesystem effects"), "{third}");
    let store = llxprt_code_rs::session::SessionStore::load_at(
        &llxprt_code_rs::session::SessionId::parse("worktree-safety").unwrap(),
        root.path(),
    )
    .unwrap();
    let snapshot = serde_json::to_value(store.snapshot().unwrap())
        .unwrap()
        .to_string();
    assert!(
        snapshot.contains("partial filesystem effects"),
        "{snapshot}"
    );
    assert!(snapshot.contains("command exited with 1"), "{snapshot}");
}
