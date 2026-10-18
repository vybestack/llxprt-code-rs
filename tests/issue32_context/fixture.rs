//! Owned, bounded HTTP fixture and file-backed CLI capture for issue #32.
#[path = "../../src/test_loopback.rs"]
mod loopback;
use llxprt_code_rs::session::{RoundRecord, SessionId, SessionState, SessionStore};
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::net::TcpListener;
use std::process::Command;

pub const GUARD: usize = 262_144 * 3;
const SESSION: &str = "task-ae84342303192f1edb23fae74512cf66";

pub struct Fixture {
    pub root: tempfile::TempDir,
    listener: TcpListener,
}

pub fn shell_reply(id: &str, bytes: usize, edit: bool) -> Value {
    let edit = if edit {
        "open('work.txt','w').write('repaired\\n'); "
    } else {
        ""
    };
    let command = format!("python3 -c \"{edit}print('test result: ok'); print('x' * {bytes})\"");
    json!({"role":"assistant","content":"Running bounded test transcripts; work is retained.",
        "tool_calls":[{"id":id,"type":"function","function":{"name":"run_shell_command",
        "arguments":json!({"command":command,"timeout_seconds":10}).to_string()}}]})
}

impl Fixture {
    pub fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();

        std::fs::write(root.path().join("work.txt"), "initial\n").unwrap();
        std::fs::write(
            root.path().join("profile.json"),
            json!({"provider":"openai","model":"fixture",
            "ephemeralSettings":{"base-url":format!("http://{}/v1", listener.local_addr().unwrap()),
            "auth-key":"public-issue32-fixture","context-limit":262144}})
            .to_string(),
        )
        .unwrap();
        Self { root, listener }
    }

    pub fn serve(
        &self,
        replies: Vec<Value>,
        live_before: Option<usize>,
    ) -> loopback::Server<Vec<Value>> {
        let listener = self.listener.try_clone().unwrap();
        let progress = self.root.path().join("large.stderr");
        loopback::spawn(listener, move |listener| {
            let mut requests = Vec::new();
            for (index, message) in replies.into_iter().enumerate() {
                let mut socket = listener.accept().unwrap().0;

                let mut reader = std::io::BufReader::new(socket.try_clone().unwrap());
                let mut length = None;
                let mut header_bytes = 0;
                loop {
                    let mut line = String::new();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    header_bytes += line.len();
                    assert!(header_bytes <= 65536);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((key, value)) = line.split_once(':') {
                        if key.eq_ignore_ascii_case("content-length") {
                            length = Some(value.trim().parse::<usize>().unwrap());
                        }
                    }
                }
                let length = length.unwrap();
                assert!(length <= GUARD, "oversized wire request: {length}");
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                requests.push(serde_json::from_slice(&body).unwrap());
                if live_before == Some(index) {
                    let events = read_events(&progress);
                    let results: Vec<_> = events
                        .iter()
                        .filter(|e| e["type"] == "tool_result")
                        .collect();
                    assert_eq!(
                        results.len(),
                        3,
                        "progress must be flushed before final reply"
                    );
                    assert!(events
                        .iter()
                        .all(|e| e["text"] != "Tests completed; retained final report."));
                }
                let finish = if message.get("tool_calls").is_some() {
                    "tool_calls"
                } else {
                    "stop"
                };
                let response = json!({"id":format!("reply-{index}"),"object":"chat.completion","created":1,"model":"fixture",
                    "choices":[{"index":0,"message":message,"finish_reason":finish}]}).to_string();
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
            }
            requests
        })
    }

    pub fn run(&self, label: &str, prompt: &str, emit: bool) -> (i32, Value, Vec<Value>) {
        let stdout = self.root.path().join(format!("{label}.stdout"));
        let stderr = self.root.path().join(format!("{label}.stderr"));
        let mut command = self.command();
        command
            .args(["--profile-load"])
            .arg(self.root.path().join("profile.json"))
            .args(["--session", SESSION, "--cwd"])
            .arg(self.root.path())
            .args([
                "--allow-shell",
                "--max-tool-calls",
                "512",
                "--max-shell-output",
                "450000",
                "--max-tool-output",
                "450000",
                "--turn-time",
                "0",
                "-p",
                prompt,
            ]);
        if emit {
            command.args(["--emit", "text,calls,results"]);
        }
        let status = command
            .stdout(std::fs::File::create(&stdout).unwrap())
            .stderr(std::fs::File::create(&stderr).unwrap())
            .status()
            .unwrap();
        // Exactly one terminal JSON object, with no extra stdout transcript.
        let terminal = serde_json::from_slice(&std::fs::read(stdout).unwrap()).unwrap();
        assert_eq!(
            self.listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
            "unexpected request after terminal outcome"
        );
        (status.code().unwrap(), terminal, read_events(&stderr))
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"));
        command
            .env("LLXPRT_CONFIG_HOME", self.root.path())
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env_remove("no_proxy");
        command
    }

    pub fn state(&self) -> SessionState {
        SessionStore::load_at(&SessionId::parse(SESSION).unwrap(), self.root.path())
            .unwrap()
            .snapshot()
            .unwrap()
    }

    pub fn lookback(&self) -> Value {
        let output = self
            .command()
            .args(["transcript", "--session", SESSION, "--json"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty());
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

fn read_events(path: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

pub fn tool_returns(request: &Value) -> Vec<&Value> {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "tool")
        .collect()
}

pub fn assert_durable_calls(rounds: &[RoundRecord], count: usize) {
    let calls: Vec<_> = rounds.iter().flat_map(|r| &r.calls).collect();
    assert_eq!(calls.len(), count);
    for call in calls {
        assert!(call.ok && !call.refused);
        assert!(call.result.contains("CTXDIGEST v1"));
        assert!(
            call.result_live.is_empty(),
            "bulk live bytes must not poison stored history"
        );
    }
}
