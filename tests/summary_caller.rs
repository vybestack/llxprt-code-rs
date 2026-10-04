//! Captured HTTP requests from the compiled CLI: summary ownership at the tool-budget seam.
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn call(id: &str, name: &str, args: Value) -> Value {
    json!({"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}})
}

fn reply(text: &str, calls: Vec<Value>, finish: &str) -> Value {
    json!({"id":"summary-fixture","object":"chat.completion","created":1,"model":"fixture",
        "choices":[{"index":0,"finish_reason":finish,"message":{
            "role":"assistant","content":text,"tool_calls":calls}}]})
}

struct Fixture {
    requests: Arc<Mutex<Vec<Value>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    address: std::net::SocketAddr,
}

impl Fixture {
    fn new(replies: Vec<Value>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let recorded = requests.clone();
        let stopping = stop.clone();
        let thread = std::thread::spawn(move || {
            while !stopping.load(std::sync::atomic::Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(5)))
                            .unwrap();
                        let mut header = Vec::new();
                        while !header.ends_with(b"\r\n\r\n") {
                            let mut byte = [0];
                            stream.read_exact(&mut byte).unwrap();
                            header.push(byte[0]);
                            assert!(header.len() <= 64 * 1024);
                        }
                        let header = String::from_utf8(header).unwrap();
                        let length: usize = header
                            .lines()
                            .find_map(|line| {
                                let (key, value) = line.split_once(':')?;
                                key.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse().unwrap())
                            })
                            .unwrap();
                        assert!(length <= 2 * 1024 * 1024);
                        let mut body = vec![0; length];
                        stream.read_exact(&mut body).unwrap();
                        let mut requests = recorded.lock().unwrap();
                        requests.push(serde_json::from_slice(&body).unwrap());
                        let response = replies
                            .get(requests.len() - 1)
                            .unwrap_or_else(|| panic!("unexpected extra request: {requests:?}"));
                        // Null deliberately withholds the response to test the shared deadline.
                        if response.is_null() {
                            drop(requests);
                            while !stopping.load(std::sync::atomic::Ordering::SeqCst) {
                                std::thread::sleep(Duration::from_millis(5));
                            }
                            continue;
                        }
                        let response = response.to_string();
                        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept: {error}"),
                }
            }
        });
        Self {
            requests,
            stop,
            thread: Some(thread),
            address,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        self.thread.take().unwrap().join().unwrap();
    }
}

struct Run {
    output: Value,
    requests: Vec<Value>,
    state: Value,
    workspace: tempfile::TempDir,
    elapsed: Duration,
    exit: i32,
}

fn run(replies: Vec<Value>, budget: u32, flags: &[&str]) -> Run {
    // These compiled fixtures perform encrypted-store/fsync setup. Keep this owning binary's
    // cases serial so its absolute deadline assertion does not include competing fixture setup.
    static SERIAL: Mutex<()> = Mutex::new(());
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new(replies);
    let workspace = tempfile::tempdir().unwrap();
    std::fs::create_dir(workspace.path().join("profiles")).unwrap();
    std::fs::write(
        workspace.path().join("profiles/fixture.json"),
        json!({
        "provider":"openai","model":"fixture","ephemeralSettings":{
            "base-url":format!("http://{}",fixture.address),"auth-key":"summary-fixture-only"
        }})
        .to_string(),
    )
    .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"));
    // Isolated dummy profile/config and no ambient proxies or auth/profile switches.
    // This matches the existing compiled deadline fixtures; production is unchanged.
    command.env_clear();
    let started = Instant::now();
    let output = command
        .env("LLXPRT_CONFIG_HOME", workspace.path())
        .args([
            "--profile",
            "fixture",
            "--session",
            "summary-caller",
            "--cwd",
        ])
        .arg(workspace.path())
        .arg("--max-tool-calls")
        .arg(budget.to_string())
        .args(flags)
        .args(["-p", "bounded summary caller proof"])
        .output()
        .unwrap();
    let elapsed = started.elapsed();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{error}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    let requests = fixture.requests.lock().unwrap().clone();
    let store = llxprt_code_rs::session::SessionStore::load_at(
        &llxprt_code_rs::session::SessionId::parse("summary-caller").unwrap(),
        workspace.path(),
    )
    .unwrap();
    let state = serde_json::to_value(store.snapshot().unwrap()).unwrap();
    Run {
        output: value,
        requests,
        state,
        workspace,
        elapsed,
        exit: output.status.code().unwrap(),
    }
}

fn mixed(preamble: &str) -> Value {
    reply(
        preamble,
        vec![
            call("unknown", "run_socket_command", json!({})),
            call(
                "write",
                "write_file",
                json!({"path":"must-not-exist","content":"bad"}),
            ),
        ],
        "tool_calls",
    )
}

fn assert_answered_batch(messages: &[Value], ids: &[&str]) {
    let assistant: Vec<_> = messages
        .iter()
        .filter(|m| {
            m["tool_calls"]
                .as_array()
                .is_some_and(|calls| !calls.is_empty())
        })
        .collect();
    assert_eq!(
        assistant.len(),
        1,
        "answered batch must not be re-appended: {messages:?}"
    );
    let calls = assistant[0]["tool_calls"].as_array().unwrap();
    assert_eq!(
        calls
            .iter()
            .map(|c| c["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ids
    );
    let results: Vec<_> = messages.iter().filter(|m| m["role"] == "tool").collect();
    assert_eq!(
        results
            .iter()
            .map(|r| r["tool_call_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ids
    );
    assert_eq!(messages[2], *assistant[0]);
    for (index, result) in results.iter().enumerate() {
        assert_eq!(messages[3 + index], **result);
    }
}

fn budget_case(preamble: &str) {
    let run = run(
        vec![mixed(preamble), reply("Final answer", vec![], "stop")],
        1,
        &[],
    );
    assert_eq!(run.exit, 0);
    assert_eq!(run.output["summary"], "Final answer");
    assert_eq!(run.output["budget_exhausted"], true);
    assert_eq!(run.output["tool_calls"], 1);
    assert!(!run.workspace.path().join("must-not-exist").exists());
    assert_eq!(run.requests.len(), 2, "preamble is not a final summary");
    let messages = run.requests[1]["messages"].as_array().unwrap();
    assert_answered_batch(messages, &["unknown", "write"]);
    assert_eq!(messages.len(), 6);
    assert_eq!(messages[2]["content"], preamble);
    assert!(messages[3]["content"]
        .as_str()
        .unwrap()
        .contains("run_socket_command"));
    assert!(messages[4]["content"].as_str().unwrap().contains("refused"));
    assert_eq!(messages[5]["role"], "user");
    assert!(messages[5]["content"]
        .as_str()
        .unwrap()
        .contains("final plain-text summary"));
    let rounds = run.state["branches"][0]["rounds"].as_array().unwrap();
    assert_eq!(rounds.len(), 2);
    let calls = rounds[0]["calls"].as_array().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["id"], "unknown");
    assert_eq!(calls[0]["ok"], false);
    assert_eq!(calls[0]["refused"], false);
    assert_eq!(calls[1]["id"], "write");
    assert_eq!(calls[1]["ok"], false);
    assert_eq!(calls[1]["refused"], true);
    assert_eq!(rounds[1]["assistant"], "Final answer");
}

#[test]
fn empty_preamble_budget_summary_has_one_answer_per_call() {
    budget_case("");
}

#[test]
fn tool_preamble_budget_summary_must_request_a_final_reply() {
    budget_case("Working on it");
}

#[test]
fn nontruncated_batch_executes_once_and_accepts_natural_no_tool_summary() {
    let run = run(
        vec![
            mixed("Working on it"),
            reply("Natural answer", vec![], "stop"),
        ],
        3,
        &[],
    );
    assert_eq!(run.exit, 0);
    assert_eq!(run.output["summary"], "Natural answer");
    assert_eq!(run.output["budget_exhausted"], false);
    assert_eq!(run.output["tool_calls"], 2);
    assert_eq!(
        std::fs::read(run.workspace.path().join("must-not-exist")).unwrap(),
        b"bad"
    );
    assert_eq!(run.requests.len(), 2);
    let messages = run.requests[1]["messages"].as_array().unwrap();
    assert_answered_batch(messages, &["unknown", "write"]);
    assert_eq!(messages.len(), 5);
}

#[test]
fn ordinary_no_tool_reply_is_the_natural_summary() {
    let run = run(vec![reply("Natural answer", vec![], "stop")], 1, &[]);
    assert_eq!(run.exit, 0);
    assert_eq!(run.output["summary"], "Natural answer");
    assert_eq!(run.requests.len(), 1);
    assert_eq!(run.output["tool_calls"], 0);
}

#[test]
fn ordinary_empty_reply_is_appended_once_before_forced_summary() {
    let run = run(
        vec![
            reply("", vec![], "stop"),
            reply("Final answer", vec![], "stop"),
        ],
        1,
        &[],
    );
    assert_eq!(run.exit, 0);
    assert_eq!(run.output["summary"], "Final answer");
    assert_eq!(run.requests.len(), 2);
    let messages = run.requests[1]["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[2]["role"], "assistant");
    assert_eq!(messages[2]["content"], "");
    assert!(messages[2].get("tool_calls").is_none());
    assert_eq!(messages[3]["role"], "user");
}

#[test]
fn empty_no_tool_reply_after_answered_batch_is_appended_once() {
    let run = run(
        vec![
            mixed("Working"),
            reply("", vec![], "stop"),
            reply("Final answer", vec![], "stop"),
        ],
        2,
        &[],
    );
    assert_eq!(run.exit, 0);
    assert_eq!(run.requests.len(), 3);
    let messages = run.requests[2]["messages"].as_array().unwrap();
    assert_answered_batch(messages, &["unknown", "write"]);
    assert_eq!(messages.len(), 7);
    assert_eq!(messages[5]["role"], "assistant");
    assert_eq!(messages[5]["content"], "");
    assert_eq!(messages[6]["role"], "user");
}

#[test]
fn budget_followup_cannot_admit_more_executable_calls() {
    let run = run(
        vec![
            mixed("Working"),
            reply(
                "More work",
                vec![call(
                    "again",
                    "write_file",
                    json!({"path":"must-not-exist","content":"bad"}),
                )],
                "tool_calls",
            ),
        ],
        1,
        &[],
    );
    assert_eq!(run.exit, 5);
    assert_eq!(run.output["error"]["code"], "invalid-tool-call");
    assert_eq!(run.requests.len(), 2);
    assert!(!run.workspace.path().join("must-not-exist").exists());
    assert_eq!(run.state["branches"][0]["lifecycle"], "failed");
    assert_eq!(
        run.state["branches"][0]["rounds"].as_array().unwrap().len(),
        1
    );
}

#[test]
fn budget_followup_control_reply_is_not_a_summary() {
    let run = run(
        vec![
            mixed("Working"),
            reply("Not complete", vec![], "content_filter"),
        ],
        1,
        &[],
    );
    assert_eq!(run.exit, 5);
    assert_eq!(run.output["error"]["code"], "finish-reason");
    assert_eq!(run.requests.len(), 2);
    assert!(!run.workspace.path().join("must-not-exist").exists());
}

#[test]
fn budget_followup_shares_the_turn_deadline_and_preserves_prefix() {
    let run = run(
        vec![mixed("Working"), Value::Null],
        1,
        &["--turn-time", "1s"],
    );
    assert_eq!(run.exit, 5);
    assert_eq!(run.output["error"]["code"], "turn-time-exhausted");
    assert_eq!(run.requests.len(), 2);
    assert!(run.elapsed < Duration::from_secs(3), "{:?}", run.elapsed);
    assert_answered_batch(
        run.requests[1]["messages"].as_array().unwrap(),
        &["unknown", "write"],
    );
    assert!(!run.workspace.path().join("must-not-exist").exists());
    assert_eq!(run.state["branches"][0]["lifecycle"], "failed");
    assert_eq!(
        run.state["branches"][0]["rounds"].as_array().unwrap().len(),
        1
    );
}
