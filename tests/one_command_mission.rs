//! Issue 89: hostile model replies must not spend prohibited exploratory calls.
#[path = "../src/test_loopback.rs"]
mod loopback;
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::net::TcpListener;
use std::process::Command;

const PROMPT: &str = "Run exactly one shell command: `python3 tmp/finisher.py`. It must print DETACHED. Then stop immediately. Do not read files and do not call another tool. The guarded script implements and verifies the monitor/cache green unit.";

fn response(calls: Vec<Value>) -> Value {
    json!({"id":"fixture","object":"chat.completion","created":1,"model":"loopback",
        "choices":[{"index":0,"finish_reason":if calls.is_empty() {"stop"} else {"tool_calls"},
            "message":{"role":"assistant","content":"I cannot inspect the ignored script.","tool_calls":calls}}]})
}

fn call(name: &str, args: Value, id: &str) -> Value {
    json!({"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}})
}

fn serve(listener: loopback::Listener, replies: Vec<Value>) -> Vec<Value> {
    // Extra replies let the unfixed loop finish, so the regression fails on the
    // invariant rather than on a hung fixture. Capture the actual request count.

    let mut requests = Vec::new();
    for reply in replies {
        let mut stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionAborted => return requests,
            Err(error) => panic!("accept: {error}"),
        };
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
        requests.push(serde_json::from_slice(&body).unwrap());
        let body = reply.to_string();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    }
    requests
}

fn run_case(
    first: Value,
    allow_shell: bool,
    budget: &str,
) -> (Value, i32, Vec<Value>, Value, bool) {
    run_with_inputs(
        first,
        allow_shell,
        budget,
        PROMPT,
        "from pathlib import Path\nPath('executed').write_text('once')\nprint('DETACHED')\n",
    )
}

fn run_with_inputs(
    first: Value,
    allow_shell: bool,
    budget: &str,
    prompt: &str,
    script: &str,
) -> (Value, i32, Vec<Value>, Value, bool) {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("profiles")).unwrap();
    std::fs::create_dir(root.path().join("tmp")).unwrap();
    std::fs::write(root.path().join(".gitignore"), "tmp/\n").unwrap();
    std::fs::write(root.path().join("tmp/finisher.py"), script).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    std::fs::write(root.path().join("profiles/loopback.json"), json!({"provider":"openai","model":"loopback","ephemeralSettings":{"base-url":format!("http://{}", listener.local_addr().unwrap()),"auth-key":"fixture"}}).to_string()).unwrap();
    let server = loopback::spawn(listener, move |listener| {
        serve(listener, vec![first, response(vec![])])
    });
    let mut cli = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"));
    cli.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("TMPDIR", root.path())
        .env("LLXPRT_CONFIG_HOME", root.path())
        .args([
            "--profile",
            "loopback",
            "--session",
            "one-command",
            "--max-tool-calls",
            budget,
            "--cwd",
        ])
        .arg(root.path())
        .args(["-p", prompt]);
    if allow_shell {
        cli.arg("--allow-shell");
    }
    let output = cli.output().unwrap();
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    let store = llxprt_code_rs::session::SessionStore::load_at(
        &llxprt_code_rs::session::SessionId::parse("one-command").unwrap(),
        root.path(),
    )
    .unwrap();
    (
        envelope,
        output.status.code().unwrap(),
        server.join().unwrap(),
        serde_json::to_value(store.snapshot().unwrap()).unwrap(),
        root.path().join("executed").exists(),
    )
}

#[test]
fn one_command_exploration_is_a_zero_call_structured_refusal() {
    let (out, code, requests, store, executed) = run_case(
        response(vec![call(
            "read_file",
            json!({"path":"tmp/finisher.py","limit":1000}),
            "read",
        )]),
        true,
        "16",
    );
    assert_eq!(out["status"], "error", "{out}");
    assert_eq!(out["error"]["code"], "one-command-refused");
    assert_eq!(out["error"]["terminal_outcome"], "one-command-refused");
    assert_eq!(code, 6);
    assert_eq!(requests.len(), 1);
    assert!(!executed);
    assert_eq!(store["branches"][0]["lifecycle"], "failed");
    assert!(store["branches"][0]["rounds"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn one_command_executes_exactly_once_and_stops_without_a_summary_request() {
    let (out, code, requests, store, executed) = run_case(
        response(vec![call(
            "run_shell_command",
            json!({"command":"python3 tmp/finisher.py","timeout_seconds":10}),
            "shell",
        )]),
        true,
        "16",
    );
    assert_eq!(code, 0, "{out}");
    assert_eq!(out["tool_calls"], 1);
    assert!(
        out["summary"].as_str().unwrap().contains("DETACHED"),
        "{out}"
    );
    assert_eq!(requests.len(), 1);
    assert!(executed);
    let rounds = store["branches"][0]["rounds"].as_array().unwrap();
    assert_eq!(rounds[0]["calls"][0]["name"], "run_shell_command");
    assert_eq!(rounds[0]["calls"][0]["ok"], true);
    let tools = requests[0]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["function"]["name"], "run_shell_command");
}

#[test]
fn one_command_rejects_mixed_batch_before_the_exact_command_runs() {
    let (out, code, requests, _, executed) = run_case(
        response(vec![
            call(
                "run_shell_command",
                json!({"command":"python3 tmp/finisher.py"}),
                "shell",
            ),
            call(
                "write_file",
                json!({"path":"extra","content":"no"}),
                "write",
            ),
        ]),
        true,
        "1",
    );
    assert_eq!(out["error"]["code"], "one-command-refused", "{out}");
    assert_eq!(code, 6);
    assert_eq!(requests.len(), 1);
    assert!(!executed);
}

#[test]
fn one_command_rejects_rewritten_command_and_plain_text_refusal() {
    for calls in [
        vec![call(
            "run_shell_command",
            json!({"command":"python3 tmp/finisher.py; touch extra"}),
            "changed",
        )],
        vec![],
    ] {
        let (out, code, requests, _, executed) = run_case(response(calls), true, "16");
        assert_eq!(out["error"]["code"], "one-command-refused", "{out}");
        assert_eq!(code, 6);
        assert_eq!(requests.len(), 1);
        assert!(!executed);
    }
}

#[test]
fn one_command_does_not_override_shell_permission_or_cli_budget_validation() {
    for (shell, budget, error, status) in [
        (false, "16", "one-command-refused", 6),
        (true, "0", "max-tool-calls", 2),
    ] {
        let (out, code, requests, _, executed) = run_case(response(vec![]), shell, budget);
        assert_eq!(out["error"]["code"], error, "{out}");
        assert_eq!(code, status);
        assert!(requests.is_empty());
        assert!(!executed);
    }
}

#[test]
fn one_command_execution_failure_is_terminal_without_retry() {
    for (script, timeout) in [
        ("raise SystemExit(7)\n", 10),
        ("import time\ntime.sleep(3)\n", 1),
    ] {
        let (out, code, requests, store, _) = run_with_inputs(
            response(vec![call(
                "run_shell_command",
                json!({"command":"python3 tmp/finisher.py", "timeout_seconds":timeout}),
                "shell",
            )]),
            true,
            "16",
            PROMPT,
            script,
        );
        assert_eq!(out["error"]["code"], "one-command-failed", "{out}");
        assert_eq!(out["error"]["terminal_outcome"], "one-command-failed");
        assert_eq!(code, 6);
        assert_eq!(requests.len(), 1);
        assert_eq!(store["branches"][0]["lifecycle"], "failed");
        assert_eq!(store["branches"][0]["rounds"][0]["calls"][0]["ok"], false);
    }
}

#[test]
fn malformed_explicit_mission_refuses_before_provider_or_tools() {
    let (out, code, requests, _, executed) = run_with_inputs(
        response(vec![]),
        true,
        "16",
        "Run exactly one shell command: python3 tmp/finisher.py",
        "print('DETACHED')\n",
    );
    assert_eq!(out["error"]["code"], "one-command-refused", "{out}");
    assert_eq!(code, 6);
    assert!(requests.is_empty());
    assert!(!executed);
}

#[test]
fn ordinary_prompt_keeps_the_normal_loop_and_tool_roster() {
    let (out, code, requests, _, executed) = run_with_inputs(
        response(vec![call(
            "run_shell_command",
            json!({"command":"python3 tmp/finisher.py"}),
            "shell",
        )]),
        true,
        "16",
        "Execute the fixture and summarize.",
        "print('DETACHED')\n",
    );
    assert_eq!(code, 0, "{out}");
    assert_eq!(requests.len(), 2);
    assert_eq!(out["summary"], "I cannot inspect the ignored script.");
    assert!(requests[0]["tools"].as_array().unwrap().len() > 1);
    assert!(!executed);
}

#[test]
fn one_command_with_no_output_still_has_a_terminal_summary() {
    let (out, code, requests, store, _) = run_with_inputs(
        response(vec![call(
            "run_shell_command",
            json!({"command":"python3 tmp/finisher.py"}),
            "shell",
        )]),
        true,
        "-1",
        PROMPT,
        "pass\n",
    );
    assert_eq!(code, 0, "{out}");
    assert_eq!(out["tool_calls"], 1);
    assert_eq!(
        out["summary"],
        "Named shell command completed successfully (no output)."
    );
    assert_eq!(requests.len(), 1);
    assert_eq!(store["branches"][0]["lifecycle"], "completed");
}
