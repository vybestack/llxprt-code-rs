//! A real worker's signal exit must retain the tool that never returned.
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn sigterm_retains_shell_evidence() {
    interrupted_shell(libc::SIGTERM, false);
}

#[test]
fn sigint_retains_shell_evidence() {
    interrupted_shell(libc::SIGINT, false);
}

#[test]
fn sigkill_leaves_pre_dispatch_tool_evidence_without_inventing_signal_marker() {
    interrupted_shell(libc::SIGKILL, false);
}

#[test]
fn sigterm_then_tool_free_continuation_retires_only_current_marker() {
    interrupted_shell(libc::SIGTERM, true);
}

fn interrupted_shell(signal: i32, continue_without_tools: bool) {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    std::fs::create_dir(root.path().join("profiles")).unwrap();
    std::fs::write(
        root.path().join("profiles/fixture.json"),
        json!({
            "provider":"openai", "model":"fixture", "ephemeralSettings": {
                "base-url":format!("http://{}",listener.local_addr().unwrap()),
                "auth-key":"fixture-secret-412"
            }
        })
        .to_string(),
    )
    .unwrap();
    let command = "echo $$ > shell.pid; echo https://user:password-412@example.com/path?token=hidden-412; sleep 60";
    let mut child = Worker(Some(
        Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
            .env_clear()
            .env("LLXPRT_CONFIG_HOME", root.path())
            .args([
                "--profile",
                "fixture",
                "--session",
                "interrupted",
                "--allow-shell",
                "-p",
                "Run the tool",
            ])
            .arg("--cwd")
            .arg(root.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    ));
    let deadline = Instant::now() + Duration::from_secs(20);
    let body = json!({"id":"r1", "object":"chat.completion", "created":1,"model":"fixture",
        "choices":[{"index":0,"finish_reason":"tool_calls","message":{"role":"assistant","content":"running",
            "tool_calls":[{"id":"shell-412","type":"function","function":{"name":"run_shell_command",
                "arguments":json!({"command":command,"timeout_seconds":120}).to_string()}}]}}]}).to_string();
    reply(&listener, deadline, &body);
    while !root.path().join("shell.pid").exists() {
        assert!(
            Instant::now() < deadline,
            "shell never started: {:?}",
            child.try_wait()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let session = root.path().join("code-rs-sessions/interrupted");
    let marker_before: Value =
        serde_json::from_slice(&std::fs::read(session.join("tool-in-flight.json")).unwrap())
            .unwrap();
    let mut before = marker_before.clone();
    before.as_object_mut().unwrap().remove("owner");
    // Always cancel and reap before assertions, including on the baseline.
    assert_eq!(unsafe { libc::kill(child.id() as i32, signal) }, 0);
    while child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "worker did not exit");
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    if signal == libc::SIGKILL {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(output.status.signal(), Some(libc::SIGKILL));
    } else {
        assert_eq!(output.status.code(), Some(128 + signal), "{output:?}");
    }
    let shell: i32 = std::fs::read_to_string(root.path().join("shell.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    while unsafe { libc::kill(shell, 0) } == 0 {
        assert!(
            Instant::now() < deadline,
            "shell survived worker cancellation"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(before["round"], 1);
    assert_eq!(before["turn"], 1);
    assert_eq!(before["tool"], "run_shell_command");
    assert!(before["started"].as_u64().unwrap() > 0);
    assert!(before["args"].as_str().unwrap().contains("sleep 60"));
    assert!(!before.to_string().contains("password-412"));
    for json_output in [true, false] {
        let mut transcript = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"));
        transcript
            .env_clear()
            .env("LLXPRT_CONFIG_HOME", root.path())
            .args(["transcript", "--session", "interrupted"]);
        if json_output {
            transcript.arg("--json");
        }
        let output = transcript.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(!text.contains("password-412"));
        if json_output {
            let transcript: Value = serde_json::from_str(&text).unwrap();
            let turn = &transcript["turns"][0];
            if signal == libc::SIGKILL {
                assert_eq!(turn["lifecycle"], "pending");
                assert!(turn["cancellation"].is_null());
                assert_eq!(
                    serde_json::from_slice::<Value>(
                        &std::fs::read(session.join("tool-in-flight.json")).unwrap()
                    )
                    .unwrap(),
                    marker_before
                );
                continue;
            }
            assert_eq!(
                turn["cancellation"]["signal"],
                if signal == libc::SIGINT {
                    "SIGINT"
                } else {
                    "SIGTERM"
                }
            );
            assert_eq!(turn["cancellation"]["in_flight"], before);
            assert_eq!(turn["lifecycle"], "cancelled");
            assert_eq!(turn["rounds"], json!([]));
        } else if signal != libc::SIGKILL {
            assert!(text.contains("cancelled"));
            assert!(text.contains("run_shell_command"));
            assert!(text.contains("sleep 60"));
        }
    }
    // Transcript reads never repair or erase an interrupted worker's marker.
    assert_eq!(
        serde_json::from_slice::<Value>(
            &std::fs::read(session.join("tool-in-flight.json")).unwrap()
        )
        .unwrap(),
        marker_before
    );
    if continue_without_tools {
        let mut continuation = Worker(Some(
            Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
                .env_clear()
                .env("LLXPRT_CONFIG_HOME", root.path())
                .args([
                    "--profile",
                    "fixture",
                    "--session",
                    "interrupted",
                    "--allow-shell",
                    "-p",
                    "Continue by answering without tools",
                ])
                .arg("--cwd")
                .arg(root.path())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        ));
        let body = json!({"id":"r2","object":"chat.completion","created":2,"model":"fixture",
            "choices":[{"index":0,"finish_reason":"stop","message":{
                "role":"assistant","content":"Finished without tools."}}]})
        .to_string();
        let deadline = Instant::now() + Duration::from_secs(20);
        reply(&listener, deadline, &body);
        while continuation.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "continuation did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = continuation.wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(envelope["status"], "ok");
        assert_eq!(envelope["tool_calls"], 0);
        assert_eq!(envelope["summary"], "Finished without tools.");
        assert!(
            !session.join("tool-in-flight.json").exists(),
            "successful zero-tool continuation left the dead predecessor in flight"
        );
        let output = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
            .env_clear()
            .env("LLXPRT_CONFIG_HOME", root.path())
            .args(["transcript", "--session", "interrupted", "--json"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let transcript: Value = serde_json::from_slice(&output.stdout).unwrap();
        let turns = transcript["turns"].as_array().unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0]["branch_id"], "b1");
        assert_eq!(turns[0]["lifecycle"], "cancelled");
        assert_eq!(turns[0]["cancellation"]["signal"], "SIGTERM");
        assert_eq!(turns[0]["cancellation"]["in_flight"], before);
        assert_eq!(turns[0]["rounds"], json!([]));
        assert_eq!(turns[1]["branch_id"], "b2");
        assert_eq!(turns[1]["lifecycle"], "completed");
        assert!(turns[1]["cancellation"].is_null());
        assert_eq!(turns[1]["summary"], "Finished without tools.");
        assert_eq!(turns[1]["rounds"][0]["calls"], json!([]));
        assert!(
            matches!(listener.accept(), Err(error)
            if error.kind() == std::io::ErrorKind::WouldBlock),
            "unexpected third provider request"
        );
    }
}

// A failed fixture assertion must never leave a worker (or its shell) behind.
struct Worker(Option<std::process::Child>);
impl std::ops::Deref for Worker {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        self.0.as_ref().unwrap()
    }
}
impl std::ops::DerefMut for Worker {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.as_mut().unwrap()
    }
}
impl Worker {
    fn wait_with_output(mut self) -> std::io::Result<std::process::Output> {
        self.0.take().unwrap().wait_with_output()
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

fn reply(listener: &TcpListener, deadline: Instant, body: &str) {
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "worker never connected");
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("{error}"),
        }
    };
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut reader = std::io::BufReader::new(&mut stream);
    let mut length = 0;
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap();
            }
        }
    }
    reader.read_exact(&mut vec![0; length]).unwrap();
    write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
}
