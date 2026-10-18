//! Stock CLI + real loopback transient failures; no provider credentials or external network.
#[path = "../src/test_loopback.rs"]
mod loopback;
use llxprt_code_rs::session::{Lifecycle, SessionId, SessionStore};
use loopback::Connection as TcpStream;
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::net::TcpListener;
use std::process::{Command, Output};
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
fn accept(listener: &loopback::Listener) -> TcpStream {
    listener.accept().unwrap().0
}
fn respond(listener: &loopback::Listener, response: Response) -> Value {
    let mut stream = accept(listener);
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
    serde_json::from_slice(&body).unwrap()
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
    fn command(&self, budget: &str) -> Command {
        self.command_with_request_timeout(budget, "900s")
    }
    fn command_with_request_timeout(&self, budget: &str, timeout: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"));
        command
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
                "--request-timeout",
                timeout,
                "--max-tool-calls",
                "4",
                "-p",
                "Inspect then summarize.",
                "--cwd",
            ])
            .arg(self.root.path());
        command
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
fn envelope(output: Output, code: i32) -> Value {
    assert_eq!(output.status.code(), Some(code), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}
#[test]
fn retry_after_recovers_after_tool_progress_without_reexecuting_tools() {
    let f = Fixture::new();
    let (raw, (first, second)) = loopback::run_listener(f.listener.try_clone().unwrap(), &mut f.command("0"), |listener, _| {
    respond(
        &listener,
        completion(vec![json!({"id":"write","type":"function",
        "function":{"name":"write_file","arguments":r#"{"path":"once","content":"checkpointed"}"#}})]),
    );
    let first = respond(&listener, throttle(Some(1)));
    let second = respond(&listener, completion(vec![]));
    (first, second)
    }).unwrap();
    let output = envelope(raw, 0);
    assert_eq!(output["status"], "ok");
    assert_eq!(output["summary"], "Recovered.");
    assert_eq!(output["tool_calls"], 1);
    assert_eq!(output["retry_attempts"], 1);
    assert_eq!(first, second);
    // The same backoff bounds are asserted with a paused clock in
    // agent::rate_limit::tests::one_second_hint_keeps_the_backoff_bounds.
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
    let (raw, requests) = loopback::run_listener(
        f.listener.try_clone().unwrap(),
        &mut f.command("0"),
        |listener, _| {
            (0..4)
                .map(|_| respond(&listener, throttle(None)))
                .collect::<Vec<_>>()
        },
    )
    .unwrap();
    let output = envelope(raw, 5);
    assert_eq!(output["error"]["code"], "model-rate-limit");
    assert_eq!(output["error"]["retry_attempts"], 3);
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
        let (raw, ()) = loopback::run_listener(
            f.listener.try_clone().unwrap(),
            &mut f.command("0"),
            |listener, _| {
                respond(
                    &listener,
                    Response {
                        status,
                        hint: Some(1),
                        body,
                    },
                );
            },
        )
        .unwrap();
        let output = envelope(raw, 5);
        assert_eq!(output["error"]["code"], code);
        assert_eq!(output["error"]["retry_attempts"], 0);
        f.no_more_requests();
    }
}
#[test]
fn deadline_and_oversized_hint_stop_before_reissue() {
    for (hint, budget, status, code, message) in [
        (7, "1s", 6, "turn-time-exhausted", "turn time budget"),
        (31, "0", 5, "model-rate-limit", "0/3 used, 3 remaining"),
    ] {
        let f = Fixture::new();
        let (raw, ()) = loopback::run_listener(
            f.listener.try_clone().unwrap(),
            &mut f.command(budget),
            |listener, _| {
                respond(&listener, throttle(Some(hint)));
            },
        )
        .unwrap();
        let output = envelope(raw, status);
        assert_eq!(output["error"]["code"], code);
        assert_eq!(output["error"]["retry_attempts"], 0);
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
    let (output, elapsed) = loopback::run_listener(
        f.listener.try_clone().unwrap(),
        &mut f.command("0"),
        |listener, pid| {
            respond(&listener, throttle(Some(30)));
            std::thread::sleep(Duration::from_millis(100));
            let start = Instant::now();
            assert_eq!(unsafe { libc::kill(pid as i32, libc::SIGTERM) }, 0);
            // Keep the signal-latency assertion at the child completion boundary.
            start
        },
    )
    .unwrap();
    assert_eq!(output.status.code(), Some(128 + libc::SIGTERM));
    assert!(elapsed.elapsed() < Duration::from_secs(2));
    f.no_more_requests();
    assert_eq!(
        f.state().snapshot().unwrap().branches[0].lifecycle,
        Lifecycle::Pending
    );
}

#[test]
fn transient_server_recovers_without_changing_the_request() {
    let f = Fixture::new();
    let (raw, (first, last)) = loopback::run_listener(
        f.listener.try_clone().unwrap(),
        &mut f.command("0"),
        |listener, _| {
            let first = respond(
                &listener,
                Response {
                    status: 503,
                    hint: None,
                    body: json!({"error":{"message":"temporarily unavailable"}}),
                },
            );
            let last = respond(&listener, completion(vec![]));
            (first, last)
        },
    )
    .unwrap();
    let output = envelope(raw, 0);
    assert_eq!(first, last);
    assert_eq!(output["retry_attempts"], 1);
    f.no_more_requests();
}

#[test]
fn connection_loss_after_request_send_is_not_replayed() {
    let f = Fixture::new();
    let (raw, ()) = loopback::run_listener(
        f.listener.try_clone().unwrap(),
        &mut f.command("0"),
        |listener, _| {
            let stream = accept(&listener);
            // Ensure the peer has received the entire request before dropping it.
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
            reader.read_exact(&mut vec![0; length]).unwrap();
            drop(reader);
            drop(stream);
        },
    )
    .unwrap();
    let output = envelope(raw, 5);
    assert_eq!(output["error"]["code"], "model-connectivity");
    assert_eq!(output["error"]["retry_attempts"], 0);
    assert!(output["error"]["message"]
        .as_str()
        .unwrap()
        .contains("request not replayed"));
    f.no_more_requests();
}

#[test]
fn anthropic_quota_refusals_and_throttles_keep_typed_retry_policy() {
    for quota in [false, true] {
        let f = Fixture::new();
        std::fs::write(
            f.root.path().join("profiles/loopback.json"),
            json!({
                "provider":"anthropic","model":"loopback","ephemeralSettings":{
                    "base-url":format!("http://{}", f.listener.local_addr().unwrap()),
                    "auth-key":"fixture-secret"
                }
            })
            .to_string(),
        )
        .unwrap();
        let (raw, ()) = loopback::run_listener(f.listener.try_clone().unwrap(), &mut f.command("0"), |listener, _| {
        respond(
            &listener,
            Response {
                status: 429,
                hint: Some(1),
                body: json!({"error":{
                    "type":if quota { "insufficient_quota" } else { "rate_limit_error" },
                    "message":if quota { "payment required" } else { "slow down" }
                }}),
            },
        );
        if !quota {
            respond(
                &listener,
                Response {
                    status: 200,
                    hint: None,
                    body: json!({
                        "id":"fixture","type":"message","role":"assistant","model":"loopback",
                        "content":[{"type":"text","text":"Recovered."}],
                        "stop_reason":"end_turn","stop_sequence":null,
                        "usage":{"input_tokens":1,"output_tokens":1}
                    }),
                },
            );
        }
        }).unwrap();
        let output = envelope(raw, 5 * i32::from(quota));
        if quota {
            assert_eq!(output["error"]["code"], "model-quota-exhausted");
            assert_eq!(output["error"]["retry_attempts"], 0);
        } else {
            assert_eq!(output["retry_attempts"], 1);
            assert_eq!(output["summary"], "Recovered.");
        }
        f.no_more_requests();
    }
}

#[test]
fn received_401_with_diagnostic_body_timeout_never_retries() {
    for provider in ["openai", "anthropic", "openai-responses"] {
        let f = Fixture::new();
        std::fs::write(
            f.root.path().join("profiles/loopback.json"),
            json!({
                "provider":provider,"model":"loopback","ephemeralSettings":{
                    "base-url":format!("http://{}", f.listener.local_addr().unwrap()),
                    "auth-key":"fixture-secret"
                }
            })
            .to_string(),
        )
        .unwrap();
        let (raw, ()) = loopback::run_listener(f.listener.try_clone().unwrap(), &mut f.command_with_request_timeout("0", "200ms"), |listener, _| {
        let mut stream = accept(&listener);
        // Read all request headers/body before supplying an incomplete error body.
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
        reader.read_exact(&mut vec![0; length]).unwrap();
        stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 100\r\nConnection: close\r\n\r\npartial").unwrap();
        // Hold the socket open: the diagnostic-body deadline, not peer EOF, fires.
        drop(reader);
        listener.until_client_exits();
        }).unwrap();
        let output = envelope(raw, 5);
        assert_eq!(output["error"]["code"], "model-permanent");
        assert_eq!(output["error"]["retry_attempts"], 0);
        assert!(output["error"]["message"]
            .as_str()
            .unwrap()
            .contains("status 401"));
        f.no_more_requests();
    }
}

#[test]
fn responses_http_retains_retry_after_and_reissues_identical_bytes() {
    let f = Fixture::new();
    std::fs::write(
        f.root.path().join("profiles/loopback.json"),
        json!({
            "provider":"openai-responses","model":"loopback","ephemeralSettings":{
                "base-url":format!("http://{}/v1/responses", f.listener.local_addr().unwrap()),
                "auth-key":"fixture-secret"
            }
        })
        .to_string(),
    )
    .unwrap();
    let (raw, (first, last)) = loopback::run_listener(f.listener.try_clone().unwrap(), &mut f.command("0"), |listener, _| {
    let first = respond(&listener, throttle(Some(1)));
    let last = respond(
        &listener,
        Response {
            status: 200,
            hint: None,
            body: json!({
                "id":"fixture","object":"response","created_at":1,"model":"loopback",
                "status":"completed","output":[{"type":"message","id":"msg","status":"completed","role":"assistant",
                    "content":[{"type":"output_text","text":"Recovered.","annotations":[]}]}],
                "usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}
            }),
        },
    );
    (first, last)
    }).unwrap();
    let output = envelope(raw, 0);
    assert_eq!(first, last);
    // Retry-After timing is pinned by the paused-clock backoff test.
    assert_eq!(output["retry_attempts"], 1);
    assert_eq!(output["summary"], "Recovered.");
    f.no_more_requests();
}
