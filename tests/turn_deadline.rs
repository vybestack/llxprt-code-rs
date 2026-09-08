//! Real CLI + silent loopback transport: no credentials or external network.
use llxprt_code_rs::session::{Lifecycle, SessionId, SessionStore};
use serde_json::Value;
use std::io::{BufRead, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn read_request(stream: &mut TcpStream) {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut reader = std::io::BufReader::new(stream);
    let mut length = None;
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("content-length") {
                length = Some(value.trim().parse::<usize>().unwrap());
            }
        }
    }
    let mut body = vec![0; length.expect("request content length")];
    reader.read_exact(&mut body).unwrap();
}

fn accept(listener: &TcpListener) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                return stream;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "CLI never connected");
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("accept failed: {error}"),
        }
    }
}

fn finish(mut child: Child) -> Output {
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!("CLI outlived turn deadline: {output:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct Fixture {
    root: tempfile::TempDir,
    listener: TcpListener,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        std::fs::create_dir(root.path().join("profiles")).unwrap();
        std::fs::write(
            root.path().join("profiles/loopback.json"),
            serde_json::json!({
                "provider": "openai", "model": "loopback",
                "ephemeralSettings": {
                    "base-url": format!("http://{}", listener.local_addr().unwrap()),
                    "auth-key": "loopback-test-only",
                    "stream-first-response-timeout-ms": 30000
                }
            })
            .to_string(),
        )
        .unwrap();
        Self { root, listener }
    }

    fn spawn(&self) -> Child {
        Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
            .env_clear()
            .env("LLXPRT_CONFIG_HOME", self.root.path())
            .args([
                "--profile",
                "loopback",
                "--session",
                "deadline",
                "--turn-time",
                "1s",
                "-p",
                "Reply OK",
            ])
            .arg("--cwd")
            .arg(self.root.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }

    fn assert_failed(&self, output: Output, rounds: usize) {
        assert_eq!(output.status.code(), Some(5), "{output:?}");
        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(envelope["error"]["code"], "turn-time-exhausted");
        let store = SessionStore::load_at(&SessionId::parse("deadline").unwrap(), self.root.path())
            .unwrap();
        let state = store.snapshot().unwrap();
        assert_eq!(state.branches[0].lifecycle, Lifecycle::Failed);
        assert_eq!(state.branches[0].rounds.len(), rounds);
        assert!(!store.session_dir().join("tool-in-flight.json").exists());
        let retry = store
            .start_request(Some(1), None, "Reply OK", self.root.path())
            .unwrap();
        assert!(retry.retry);
    }
}

fn assert_connection_closed(mut stream: TcpStream) {
    let mut byte = [0];
    match stream.read(&mut byte) {
        Ok(0) => {}
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
        other => panic!("cancelled provider connection still open: {other:?}"),
    }
}

#[test]
fn cli_initial_silent_request_emits_timeout_and_releases_session() {
    let f = Fixture::new();
    let child = f.spawn();
    let mut stream = accept(&f.listener);
    read_request(&mut stream);
    f.assert_failed(finish(child), 0);
    assert_connection_closed(stream);
}

#[test]
fn cli_subsequent_silent_request_retains_completed_tool_round() {
    let f = Fixture::new();
    let child = f.spawn();
    let mut first = accept(&f.listener);
    read_request(&mut first);
    let body = serde_json::json!({
        "id":"r1", "object":"chat.completion", "created":1, "model":"loopback",
        "choices":[{"index":0, "finish_reason":"tool_calls", "message":{
            "role":"assistant", "content":"list", "tool_calls":[{
                "id":"list-1", "type":"function", "function":{
                    "name":"list_directory", "arguments":"{\"path\":\".\"}"
                }
            }]
        }}]
    })
    .to_string();
    write!(first, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    drop(first);
    let mut second = accept(&f.listener);
    read_request(&mut second);
    f.assert_failed(finish(child), 1);
    assert_connection_closed(second);
}

#[test]
fn sigterm_during_provider_request_keeps_existing_signal_exit_semantics() {
    let f = Fixture::new();
    let child = f.spawn();
    let mut stream = accept(&f.listener);
    read_request(&mut stream);
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    let output = finish(child);
    assert_eq!(output.status.code(), Some(128 + libc::SIGTERM));
    assert_connection_closed(stream);
    // Signal cancellation exits immediately, unlike a budget failure; it does not
    // manufacture a completed/failed transcript. The session lease is reclaimed later.
    let store =
        SessionStore::load_at(&SessionId::parse("deadline").unwrap(), f.root.path()).unwrap();
    assert_eq!(
        store.snapshot().unwrap().branches[0].lifecycle,
        Lifecycle::Pending
    );
}

#[test]
fn timed_out_turn_teardown_cancels_spawned_transport_work() {
    // Codex SSE drives its reader on a Tokio task. The turn executor must own
    // that task even when dropping the outer request alone cannot cancel it.
    use llxprt_code_rs::adapter::{ChatBackend, ModelFuture};
    use llxprt_code_rs::agent::CodingAgent;
    struct SpawnedReader(mpsc::Sender<()>);
    struct Dropped(mpsc::Sender<()>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.send(()).unwrap();
        }
    }
    impl ChatBackend for SpawnedReader {
        fn request<'a>(
            &'a self,
            _: &'a [serdes_ai::core::ModelRequest],
            _: &'a [llxprt_code_rs::tools::ToolSpec],
        ) -> ModelFuture<'a> {
            Box::pin(async move {
                let sender = self.0.clone();
                tokio::spawn(async move {
                    let _guard = Dropped(sender);
                    std::future::pending::<()>().await;
                });
                tokio::task::yield_now().await;
                tokio::time::sleep(Duration::from_secs(1)).await;
                Err(llxprt_code_rs::adapter::ModelFailure::Terminal(
                    "outer request completed instead of being cancelled".into(),
                ))
            })
        }
    }
    let (sender, receiver) = mpsc::channel();
    let root = tempfile::tempdir().unwrap();
    let agent =
        CodingAgent::with_backend(Box::new(SpawnedReader(sender)), root.path().into(), false)
            .with_turn_time(Some(Duration::from_millis(100)));
    let store =
        SessionStore::load_at(&SessionId::parse("spawned-reader").unwrap(), root.path()).unwrap();
    let reserved = store.start_request(None, None, "P", root.path()).unwrap();
    assert_eq!(
        agent.run(&store, &reserved).unwrap_err().key,
        "turn-time-exhausted"
    );
    receiver.recv_timeout(Duration::from_secs(1)).unwrap();
}
