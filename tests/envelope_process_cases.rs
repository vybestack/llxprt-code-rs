use llxprt_code_rs::cli::Code;
use serde::Deserialize;
use serde_json::Value;
use std::io::{BufRead, Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessCase {
    case: String,
    expected_exit: i32,
    expected_status: String,
}

fn bin() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"));
    for name in [
        "LLXPRT_CONFIG_HOME",
        "LLXPRT_CONFIG_DIR",
        "XDG_CONFIG_HOME",
        "OPENAI_API_KEY",
    ] {
        command.env_remove(name);
    }
    command
}

fn profile(root: &std::path::Path, endpoint: std::net::SocketAddr) {
    std::fs::create_dir_all(root.join("profiles")).unwrap();
    std::fs::write(
        root.join("profiles/loopback.json"),
        serde_json::json!({
            "provider": "openai",
            "model": "fixture-model",
            "ephemeralSettings": {
                "base-url": format!("http://{endpoint}"),
                "auth-key": "fixture-secret-not-a-real-key"
            }
        })
        .to_string(),
    )
    .unwrap();
}

struct ErrorServer {
    address: SocketAddr,
    shutdown: Option<mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ErrorServer {
    fn address(&self) -> SocketAddr {
        self.address
    }
}

impl Drop for ErrorServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

fn error_server() -> ErrorServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown, shutdown_rx) = mpsc::channel();
    let thread = std::thread::spawn(move || loop {
        if shutdown_rx.try_recv().is_ok() {
            break;
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                let reader_stream = match stream.try_clone() {
                    Ok(reader_stream) => reader_stream,
                    Err(error) => {
                        eprintln!("error fixture server could not clone stream: {error}");
                        continue;
                    }
                };
                let mut reader = std::io::BufReader::new(reader_stream);
                let mut saw_post = false;
                let mut first_line = true;
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) => break,
                        Ok(_) if line == "\r\n" => break,
                        Ok(_) => {
                            if first_line && line.starts_with("POST ") {
                                saw_post = true;
                            }
                            first_line = false;
                        }
                        Err(error) => {
                            eprintln!("error fixture server request read failed: {error}");
                            break;
                        }
                    }
                }
                if saw_post {
                    let body = r#"{"error":"isolated fixture failure"}"#;
                    let _ = write!(
                        stream,
                        "HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => eprintln!("error fixture server accept failed: {error}"),
        }
    });
    ErrorServer {
        address,
        shutdown: Some(shutdown),
        thread: Some(thread),
    }
}

fn run_bounded(mut command: Command) -> Output {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            if let Some(mut pipe) = child.stdout.take() {
                let _ = pipe.read_to_end(&mut stdout);
            }
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_end(&mut stderr);
            }
            panic!(
                "CLI exceeded 120-second deadline\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&stdout),
                String::from_utf8_lossy(&stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn invoke(root: &std::path::Path, args: &[&str]) -> Output {
    let mut command = bin();
    command.env("LLXPRT_CONFIG_HOME", root).args(args);
    run_bounded(command)
}

fn assert_error(output: &Output, expected_exit: i32) {
    assert_eq!(output.status.code(), Some(expected_exit));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["status"], "error");
    assert!(value["session_id"].is_string());
    assert!(value["error"]["code"].is_string());
    assert!(value["error"]["message"].is_string());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("fixture-secret-not-a-real-key"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("fixture-secret-not-a-real-key"));
}

#[test]
fn isolated_loopback_processes_cover_every_public_error_exit_class() {
    let cases: Vec<ProcessCase> =
        serde_json::from_str(include_str!("fixtures/envelope/process-cases.json")).unwrap();
    let expected = [
        ("usage", Code::Usage as i32),
        ("config", Code::Config as i32),
        ("session", Code::Session as i32),
        ("model", Code::Model as i32),
        ("turn", Code::Turn as i32),
    ];
    assert_eq!(cases.len(), expected.len(), "process fixture/enum drift");
    for (case, (name, exit)) in cases.iter().zip(expected) {
        assert_eq!(case.case, name);
        assert_eq!(case.expected_exit, exit);
        assert_eq!(case.expected_status, "error");
    }

    let mut usage = bin();
    usage.arg("--unknown-fixture-flag");
    assert_error(&run_bounded(usage), Code::Usage as i32);

    let config_root = tempfile::tempdir().unwrap();
    assert_error(
        &invoke(
            config_root.path(),
            &["--profile", "missing", "-p", "fixture"],
        ),
        Code::Config as i32,
    );

    let session_root = tempfile::tempdir().unwrap();
    let session_server = error_server();
    profile(session_root.path(), session_server.address());
    std::fs::write(
        session_root.path().join("code-rs-sessions"),
        "not a directory",
    )
    .unwrap();
    assert_error(
        &invoke(
            session_root.path(),
            &["--profile", "loopback", "-p", "fixture"],
        ),
        Code::Session as i32,
    );
    drop(session_server);

    let model_root = tempfile::tempdir().unwrap();
    let model_server = error_server();
    profile(model_root.path(), model_server.address());
    assert_error(
        &invoke(
            model_root.path(),
            &["--profile", "loopback", "-p", "fixture"],
        ),
        Code::Model as i32,
    );
    drop(model_server);

    let turn_root = tempfile::tempdir().unwrap();
    let turn_server = error_server();
    profile(turn_root.path(), turn_server.address());
    assert_error(
        &invoke(
            turn_root.path(),
            &["--profile", "loopback", "--turn", "2", "-p", "fixture"],
        ),
        Code::Turn as i32,
    );
    drop(turn_server);
}

#[test]
fn hostile_usage_session_still_emits_a_schema_valid_envelope() {
    let mut command = bin();
    command.args([
        "--session",
        "../../hostile session",
        "--unknown-fixture-flag",
    ]);
    let output = run_bounded(command);
    assert_error(&output, Code::Usage as i32);

    let schema: Value =
        serde_json::from_slice(include_bytes!("../docs/envelope.schema.json")).unwrap();
    let validator = jsonschema::draft202012::new(&schema).unwrap();
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    let session_id = envelope["session_id"].as_str().unwrap();
    assert!(
        session_id.starts_with("session-"),
        "usage envelope should use a fresh fallback session id, got {session_id:?}"
    );
    assert!(
        validator.is_valid(&envelope),
        "usage envelope failed published schema: {:?}",
        validator.iter_errors(&envelope).collect::<Vec<_>>()
    );
}

#[test]
fn omitted_session_with_end_of_options_keeps_generated_identity_in_error_envelope() {
    let root = tempfile::tempdir().unwrap();
    let output = invoke(root.path(), &["--"]);
    assert_error(&output, Code::Config as i32);
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let session_id = envelope["session_id"].as_str().unwrap();
    assert!(llxprt_code_rs::session::SessionId::parse(session_id).is_ok());
    assert!(session_id.starts_with("session-"));
    assert!(session_id[8..]
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
}

#[test]
fn session_shaped_text_after_end_of_options_does_not_select_envelope_session() {
    for trailing in [
        vec!["--", "--session", "named"],
        vec!["--", "--session=named"],
    ] {
        let root = tempfile::tempdir().unwrap();
        let output = invoke(root.path(), &trailing);
        assert_error(&output, 2);
        let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let session_id = envelope["session_id"].as_str().unwrap();
        assert_ne!(session_id, "named");
        assert!(llxprt_code_rs::session::SessionId::parse(session_id).is_ok());
        assert!(session_id.starts_with("session-"));
        assert!(session_id[8..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
    }
}
