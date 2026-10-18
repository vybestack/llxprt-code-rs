//! A real worker's signal exit must retain the tool that never returned.
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn sigterm_retains_shell_evidence() {
    interrupted_shell(libc::SIGTERM);
}

#[test]
fn sigint_retains_shell_evidence() {
    interrupted_shell(libc::SIGINT);
}

#[test]
fn sigkill_leaves_pre_dispatch_tool_evidence_without_inventing_signal_marker() {
    interrupted_shell(libc::SIGKILL);
}

fn interrupted_shell(signal: i32) {
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
    let mut child = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
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
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
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
    let body = json!({"id":"r1", "object":"chat.completion", "created":1,"model":"fixture",
        "choices":[{"index":0,"finish_reason":"tool_calls","message":{"role":"assistant","content":"running",
            "tool_calls":[{"id":"shell-412","type":"function","function":{"name":"run_shell_command",
                "arguments":json!({"command":command,"timeout_seconds":120}).to_string()}}]}}]}).to_string();
    write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
    drop(stream);
    while !root.path().join("shell.pid").exists() {
        assert!(
            Instant::now() < deadline,
            "shell never started: {:?}",
            child.try_wait()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let session = root.path().join("code-rs-sessions/interrupted");
    let before: Value =
        serde_json::from_slice(&std::fs::read(session.join("tool-in-flight.json")).unwrap())
            .unwrap();
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
                    before
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
}
