//! Stock CLI + real loopback 429s; no provider credentials or external network.
use llxprt_code_rs::session::{Lifecycle, SessionId, SessionStore};
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Response {
    status: u16,
    hint: Option<u64>,
    body: Value,
}
fn throttle(seconds: Option<u64>) -> Response {
    Response {
        status: 429,
        hint: seconds,
        body: json!({"error":{"message":"slow down"}}),
    }
}
fn completion(calls: Vec<Value>) -> Response {
    let finish = if calls.is_empty() {
        "stop"
    } else {
        "tool_calls"
    };
    Response {
        status: 200,
        hint: None,
        body: json!({
            "id":"fixture","object":"chat.completion","created":1,"model":"loopback",
            "choices":[{"index":0,"message":{"role":"assistant","content":"Recovered.",
                "tool_calls":calls},"finish_reason":finish}]
        }),
    }
}
fn accept(listener: &TcpListener) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                return stream;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "missing expected retry");
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("accept: {error}"),
        }
    }
}
fn respond(listener: &TcpListener, response: Response) -> (Value, Instant) {
    let mut stream = accept(listener);
    let now = Instant::now();
    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
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
    let mut body = vec![0; length.unwrap()];
    reader.read_exact(&mut body).unwrap();
    let hint = response
        .hint
        .map(|s| format!("Retry-After: {s}\r\n"))
        .unwrap_or_default();
    let payload = response.body.to_string();
    write!(stream, "HTTP/1.1 {} Fixture\r\nContent-Type: application/json\r\n{hint}Content-Length: {}\r\nConnection: close\r\n\r\n{payload}", response.status, payload.len()).unwrap();
    (serde_json::from_slice(&body).unwrap(), now)
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
            json!({
                "provider":"openai","model":"loopback","ephemeralSettings":{
                    "base-url":format!("http://{}", listener.local_addr().unwrap()),
                    "auth-key":"fixture-secret"
                }
            })
            .to_string(),
        )
        .unwrap();
        Self { root, listener }
    }
    fn spawn(&self, budget: &str) -> Child {
        Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
            .env_clear()
            .env("LLXPRT_CONFIG_HOME", self.root.path())
            .env("NO_PROXY", "127.0.0.1,localhost")
            .args([
                "--profile",
                "loopback",
                "--session",
                "rate-limit",
                "--turn-time",
                budget,
                "--max-tool-calls",
                "4",
                "-p",
                "Inspect then summarize.",
                "--cwd",
            ])
            .arg(self.root.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }
    fn state(&self) -> llxprt_code_rs::session::SessionStore {
        SessionStore::load_at(&SessionId::parse("rate-limit").unwrap(), self.root.path()).unwrap()
    }
    fn no_more_requests(&self) {
        assert!(
            matches!(self.listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock)
        );
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
            panic!(
                "CLI did not terminate: {:?}",
                child.wait_with_output().unwrap()
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn envelope(output: Output, code: i32) -> Value {
    assert_eq!(output.status.code(), Some(code), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}
#[test]
fn retry_after_recovers_after_tool_progress_without_reexecuting_tools() {
    let f = Fixture::new();
    let child = f.spawn("10s");
    respond(
        &f.listener,
        completion(vec![json!({"id":"write","type":"function",
        "function":{"name":"write_file","arguments":r#"{"path":"once","content":"checkpointed"}"#}})]),
    );
    let (first, failed_at) = respond(&f.listener, throttle(Some(1)));
    let (second, retried_at) = respond(&f.listener, completion(vec![]));
    let output = envelope(finish(child), 0);
    assert_eq!(output["status"], "ok");
    assert_eq!(output["summary"], "Recovered.");
    assert_eq!(output["tool_calls"], 1);
    assert_eq!(first, second);
    assert!(retried_at.duration_since(failed_at) >= Duration::from_secs(1));
    assert!(retried_at.duration_since(failed_at) < Duration::from_secs(5));
    assert!(second.to_string().contains("checkpointed"));
    let state = f.state().snapshot().unwrap();
    assert_eq!(state.branches[0].lifecycle, Lifecycle::Completed);
    // Only the tool round and final summary persist; the failed 429 does not.
    assert_eq!(state.branches[0].rounds.len(), 2);
    assert_eq!(
        state.branches[0]
            .rounds
            .iter()
            .map(|r| r.calls.len())
            .sum::<usize>(),
        1
    );
    assert_eq!(
        std::fs::read_to_string(f.root.path().join("once")).unwrap(),
        "checkpointed"
    );
    f.no_more_requests();
}
#[test]
fn repeated_429_exhausts_three_retries_and_exposes_budget_in_envelope() {
    let f = Fixture::new();
    let child = f.spawn("10s");
    let requests = (0..4)
        .map(|_| respond(&f.listener, throttle(None)).0)
        .collect::<Vec<_>>();
    let output = envelope(finish(child), 5);
    assert_eq!(output["error"]["code"], "model-rate-limit");
    let message = output["error"]["message"].as_str().unwrap();
    assert!(message.contains("retry cap exhausted"));
    assert!(message.contains("3/3 used, 0 remaining"));
    assert!(requests.iter().all(|request| request == &requests[0]));
    assert_eq!(
        f.state().snapshot().unwrap().branches[0].lifecycle,
        Lifecycle::Failed
    );
    f.no_more_requests();
}
#[test]
fn quota_and_permanent_errors_do_not_retry() {
    for (status, body, code) in [
        (
            429,
            json!({"error":{"code":"insufficient_quota","message":"billing limit"}}),
            "model-quota-exhausted",
        ),
        (
            401,
            json!({"error":{"message":"unauthorized"}}),
            "model-permanent",
        ),
    ] {
        let f = Fixture::new();
        let child = f.spawn("10s");
        respond(
            &f.listener,
            Response {
                status,
                hint: Some(1),
                body,
            },
        );
        let output = envelope(finish(child), 5);
        assert_eq!(output["error"]["code"], code);
        f.no_more_requests();
    }
}
#[test]
fn deadline_and_oversized_hint_stop_before_reissue() {
    for (hint, budget, code, message) in [
        (7, "1s", "turn-time-exhausted", "turn time budget"),
        (31, "10s", "model-rate-limit", "0/3 used, 3 remaining"),
    ] {
        let f = Fixture::new();
        let child = f.spawn(budget);
        respond(&f.listener, throttle(Some(hint)));
        let output = envelope(finish(child), 5);
        assert_eq!(output["error"]["code"], code);
        assert!(output["error"]["message"]
            .as_str()
            .unwrap()
            .contains(message));
        f.no_more_requests();
        assert_eq!(
            f.state().snapshot().unwrap().branches[0].lifecycle,
            Lifecycle::Failed
        );
    }
}
#[test]
fn sigterm_interrupts_retry_after_immediately() {
    let f = Fixture::new();
    let child = f.spawn("0");
    respond(&f.listener, throttle(Some(30)));
    std::thread::sleep(Duration::from_millis(100));
    let start = Instant::now();
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    let output = finish(child);
    assert_eq!(output.status.code(), Some(128 + libc::SIGTERM));
    assert!(start.elapsed() < Duration::from_secs(2));
    f.no_more_requests();
    assert_eq!(
        f.state().snapshot().unwrap().branches[0].lifecycle,
        Lifecycle::Pending
    );
}
