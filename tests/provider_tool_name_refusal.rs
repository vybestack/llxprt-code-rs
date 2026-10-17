//! Real CLI + structured OpenAI replies: provider name fields, not assistant prose.
use llxprt_code_rs::session::{Lifecycle, SessionId, SessionStore};
use serde_json::{json, Value};
use std::net::TcpListener;
use std::process::Command;

#[path = "provider_tool_name_refusal/fixture.rs"]
mod fixture;

fn tool(id: &str, name: &str, args: &str) -> Value {
    json!({"id":id,"type":"function","function":{"name":name,"arguments":args}})
}

fn completion(text: &str, calls: Vec<Value>) -> Value {
    let finish = if calls.is_empty() {
        "stop"
    } else {
        "tool_calls"
    };
    json!({"id":"fixture","object":"chat.completion","created":1,"model":"loopback",
        "choices":[{"index":0,"message":{"role":"assistant","content":text,"tool_calls":calls},"finish_reason":finish}]})
}

fn run(replies: Vec<Value>, expected_code: i32) -> (Value, Vec<Value>, tempfile::TempDir) {
    run_with_flags(
        replies,
        expected_code,
        &["--max-tool-calls", "4", "--allow-shell"],
    )
}

fn run_with_flags(
    replies: Vec<Value>,
    expected_code: i32,
    flags: &[&str],
) -> (Value, Vec<Value>, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    std::fs::create_dir(root.path().join("profiles")).unwrap();
    std::fs::write(
        root.path().join("profiles/loopback.json"),
        json!({
            "provider":"openai","model":"loopback","ephemeralSettings":{
                "base-url":format!("http://{}", listener.local_addr().unwrap()),
                "auth-key":"configured-fixture-secret"
            }
        })
        .to_string(),
    )
    .unwrap();
    let (output, captured) = fixture::run(
        listener,
        replies,
        Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
            .env_clear()
            .env("LLXPRT_CONFIG_HOME", root.path())
            .env("NO_PROXY", "127.0.0.1,localhost")
            .args([
                "--profile",
                "loopback",
                "--session",
                "name-refusal",
                "--turn-time",
                // These are refusal/budget tests, not wall-clock deadline tests.
                // Paused-clock deadline coverage lives in src/agent/deadline/tests.rs.
                "0",
                "-p",
                "Inspect the workspace, then summarize.",
                "--cwd",
            ])
            .arg(root.path())
            .args(flags),
    )
    .unwrap_or_else(|error| panic!("provider fixture: {error}"));
    assert_eq!(output.status.code(), Some(expected_code), "{output:?}");
    let envelope = serde_json::from_slice(&output.stdout).unwrap();
    (envelope, captured, root)
}

fn store(root: &std::path::Path) -> SessionStore {
    SessionStore::load_at(&SessionId::parse("name-refusal").unwrap(), root).unwrap()
}

#[test]
fn oversized_provider_name_refuses_batch_then_corrects_in_same_turn() {
    let name = "x".repeat(65);
    let refused = completion(
        "",
        vec![
            tool(
                "write",
                "write_file",
                r#"{"path":"must-not-exist","content":"private-arg"}"#,
            ),
            tool("bad", &name, "{}"),
        ],
    );
    let (envelope, requests, root) = run(
        vec![
            refused,
            completion("", vec![tool("read", "list_directory", r#"{"path":"."}"#)]),
            completion("Done.", vec![]),
        ],
        0,
    );
    assert_eq!(envelope["status"], "ok");
    assert_eq!(requests.len(), 3);
    let second = requests[1].to_string();
    assert!(second.contains("previous tool-call reply was refused"));
    assert!(!second.contains(&name));
    assert!(!second.contains("private-arg"));
    assert!(!root.path().join("must-not-exist").exists());
    let snapshot = store(root.path()).snapshot().unwrap();
    assert_eq!(snapshot.branches[0].lifecycle, Lifecycle::Completed);
    let calls = snapshot.branches[0]
        .rounds
        .iter()
        .flat_map(|r| &r.calls)
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "list_directory");
}

#[test]
fn repeated_invalid_name_has_typed_terminal_and_sanitized_source_after_work() {
    let name = format!(
        "bad\n\u{1b} https://user:private-password@host/path?token=private-query {}",
        "界".repeat(200)
    );
    let rejected = || completion("", vec![tool("bad", &name, r#"{"unused":"private-arg"}"#)]);
    let (envelope, requests, root) = run(
        vec![
            completion("", vec![tool("read", "list_directory", r#"{"path":"."}"#)]),
            rejected(),
            rejected(),
        ],
        5,
    );
    assert_eq!(requests.len(), 3);
    assert_eq!(envelope["error"]["code"], "malformed_tool_call");
    assert_eq!(envelope["error"]["terminal_outcome"], "malformed_tool_call");
    let message = envelope["error"]["message"].as_str().unwrap();
    assert!(message.contains("tool name exceeds the 64 byte cap"));
    assert!(message.contains("source=mapped provider tool_calls[0].name"));
    assert!(message.contains(&format!("bytes=0..{}", name.len())));
    assert!(message.contains("[truncated]"));
    assert!(message.contains("\\n\\u001b"));
    assert!(!message.chars().any(char::is_control));
    for private in ["private-password", "private-query", "private-arg"] {
        assert!(!message.contains(private));
    }
    let store = store(root.path());
    let snapshot = store.snapshot().unwrap();
    assert_eq!(snapshot.branches[0].lifecycle, Lifecycle::Failed);
    assert_eq!(snapshot.branches[0].rounds.len(), 1);
    assert!(!store.session_dir().join("tool-in-flight.json").exists());
    let retry = store
        .start_request(
            Some(1),
            None,
            "Inspect the workspace, then summarize.",
            root.path(),
        )
        .unwrap();
    assert!(retry.retry);
}

#[test]
fn ordinary_prose_with_long_identifier_never_becomes_a_tool_name() {
    let text = format!(
        "The identifier {} is an example, not a tool invocation.",
        "identifier".repeat(40)
    );
    let (envelope, requests, root) = run(vec![completion(&text, vec![])], 0);
    assert_eq!(envelope["summary"], text);
    assert_eq!(requests.len(), 1);
    assert_eq!(
        store(root.path()).snapshot().unwrap().branches[0].lifecycle,
        Lifecycle::Completed
    );
}

#[test]
fn secret_in_later_call_prevents_reissue_and_diagnostic_excerpt() {
    let name = "x".repeat(65);
    let (envelope, requests, root) = run(
        vec![completion(
            "",
            vec![
                tool("bad", &name, "{}"),
                tool(
                    "later",
                    "read_file",
                    r#"{"path":"configured-fixture-secret"}"#,
                ),
            ],
        )],
        5,
    );
    assert_eq!(requests.len(), 1);
    assert_eq!(envelope["error"]["code"], "model");
    let message = envelope["error"]["message"].as_str().unwrap();
    assert!(message.contains("configured secret"));
    assert!(!message.contains("configured-fixture-secret"));
    assert!(!message.contains(&name));
    assert!(store(root.path()).snapshot().unwrap().branches[0]
        .rounds
        .is_empty());
}

// Issue 206 is already integrated on main. These compiled-provider cases pin the
// original unlimited-budget failure and the capability/batch-admission boundaries.
#[test]
fn unknown_name_after_investigation_corrects_to_shell_without_losing_the_turn() {
    let investigation = (0..41)
        .map(|index| {
            tool(
                &format!("inspect-{index}"),
                "list_directory",
                r#"{"path":"."}"#,
            )
        })
        .collect();
    let args = r#"{"command":"printf corrected > corrected","timeout_seconds":1}"#;
    let (envelope, requests, root) = run_with_flags(
        vec![
            completion("Investigating", investigation),
            completion("", vec![tool("typo", "run_socket_command", args)]),
            completion("", vec![tool("corrected", "run_shell_command", args)]),
            completion("Corrected in the same turn.", vec![]),
        ],
        0,
        &["--max-tool-calls=-1", "--allow-shell"],
    );
    assert_eq!(envelope["status"], "ok");
    assert_eq!(envelope["summary"], "Corrected in the same turn.");
    assert_eq!(envelope["turn"], 1);
    assert_eq!(envelope["attempt"], 1);
    assert_eq!(envelope["tool_calls"], 43);
    assert_eq!(envelope["declared_tool_calls"], -1);
    assert_eq!(envelope["budget_exhausted"], false);
    assert_eq!(requests.len(), 4);
    let messages = requests[2]["messages"].as_array().unwrap();
    let failure = messages.last().unwrap();
    assert_eq!(failure["role"], "tool");
    assert_eq!(failure["tool_call_id"], "typo");
    let wire = failure["content"].as_str().unwrap();
    let quoted = wire.strip_prefix("Error: ").unwrap();
    let environment: llxprt_code_rs::context_kernel::security::EnvironmentEnvelope =
        serde_json::from_str(quoted).expect("strict environment envelope");
    assert!(environment.valid());
    assert_eq!(quoted, environment.render());
    let content = environment.quoted_data.as_str();
    assert!(content.contains("unknown or disabled tool run_socket_command"));
    assert!(content.contains("available:"));
    assert!(content.contains("run_shell_command"));
    assert!(content.len() <= envelope["output_caps"]["tool"].as_u64().unwrap() as usize);
    assert!(!requests[2]
        .to_string()
        .contains("previous tool-call reply was refused"));
    assert_eq!(
        std::fs::read(root.path().join("corrected")).unwrap(),
        b"corrected"
    );
    let state = store(root.path()).snapshot().unwrap();
    let branch = &state.branches[0];
    assert_eq!(branch.lifecycle, Lifecycle::Completed);
    assert_eq!(branch.rounds[0].calls.len(), 41);
    let bad = &branch.rounds[1].calls[0];
    assert_eq!(
        (&*bad.id, &*bad.name, &*bad.args),
        ("typo", "run_socket_command", args)
    );
    assert_eq!((bad.ok, bad.refused), (false, false));
    assert_eq!(bad.result, content);
    assert!(bad.result_live.is_empty());
    assert!(branch.rounds[2].calls[0].ok);
}

#[test]
fn disabled_shell_returns_bounded_failure_and_cannot_execute_under_unlimited_budget() {
    let (envelope, requests, root) = run_with_flags(
        vec![
            completion(
                "",
                vec![tool(
                    "disabled",
                    "run_shell_command",
                    r#"{"command":"touch must-not-exist","timeout_seconds":1}"#,
                )],
            ),
            completion("", vec![tool("safe", "list_directory", r#"{"path":"."}"#)]),
            completion("Done safely.", vec![]),
        ],
        0,
        &["--max-tool-calls=-1", "--max-tool-output", "128"],
    );
    assert_eq!(envelope["status"], "ok");
    assert_eq!(envelope["tool_calls"], 2);
    assert!(!root.path().join("must-not-exist").exists());
    assert_eq!(requests.len(), 3);
    assert!(requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .all(|spec| spec["function"]["name"] != "run_shell_command"));
    let messages = requests[1]["messages"].as_array().unwrap();
    let failure = messages.last().unwrap();
    assert_eq!(failure["role"], "tool");
    assert_eq!(failure["tool_call_id"], "disabled");
    let wire = failure["content"].as_str().unwrap();
    let quoted = wire.strip_prefix("Error: ").unwrap();
    let environment: llxprt_code_rs::context_kernel::security::EnvironmentEnvelope =
        serde_json::from_str(quoted).expect("strict environment envelope");
    assert!(environment.valid());
    assert_eq!(quoted, environment.render());
    let content = environment.quoted_data.as_str();
    assert!(content.contains("unknown or disabled tool run_shell_command"));
    assert!(content.len() <= 128);
    assert!(format!("Error: {content}").len() <= 128 + "Error: ".len());
    assert_eq!(wire, format!("Error: {}", environment.render()));
    assert!(!content.contains("touch"));
    assert!(!content
        .split("available: ")
        .nth(1)
        .unwrap()
        .contains("run_shell_command"));
    let state = store(root.path()).snapshot().unwrap();
    assert_eq!(state.branches[0].lifecycle, Lifecycle::Completed);
    let bad = &state.branches[0].rounds[0].calls[0];
    assert_eq!((bad.ok, bad.refused), (false, false));
    assert_eq!(bad.result, content);
}

#[test]
fn repeated_unknown_names_consume_the_existing_call_budget() {
    let (envelope, requests, root) = run_with_flags(
        vec![
            completion("", vec![tool("one", "run_socket_command", "{}")]),
            completion(
                "",
                vec![
                    tool("two", "run_socket_command", "{}"),
                    tool(
                        "three",
                        "write_file",
                        r#"{"path":"must-not-exist","content":"bad"}"#,
                    ),
                ],
            ),
            completion("Budget exhausted.", vec![]),
        ],
        0,
        &["--max-tool-calls", "2"],
    );
    assert_eq!(envelope["tool_calls"], 2);
    assert_eq!(envelope["budget_exhausted"], true);
    assert_eq!(requests.len(), 3);
    assert!(!root.path().join("must-not-exist").exists());
    let messages = requests[2]["messages"].as_array().unwrap();
    let results = messages
        .iter()
        .filter(|m| m["role"] == "tool")
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 3);
    assert_eq!(results[1]["tool_call_id"], "two");
    assert!(results[1]["content"]
        .as_str()
        .unwrap()
        .contains("unknown or disabled"));
    assert_eq!(results[2]["tool_call_id"], "three");
    assert!(results[2]["content"]
        .as_str()
        .unwrap()
        .contains("tool-call budget exhausted"));
    let state = store(root.path()).snapshot().unwrap();
    let calls = &state.branches[0].rounds[1].calls;
    assert_eq!((calls[0].ok, calls[0].refused), (false, false));
    assert_eq!((calls[1].ok, calls[1].refused), (false, true));
}

#[test]
fn malformed_unknown_call_refuses_whole_provider_batch_before_any_side_effect() {
    for args in ["not json", "[]", "null"] {
        let (envelope, requests, root) = run_with_flags(
            vec![completion(
                "",
                vec![
                    tool(
                        "write",
                        "write_file",
                        r#"{"path":"must-not-exist","content":"bad"}"#,
                    ),
                    tool("bad", "run_socket_command", args),
                ],
            )],
            5,
            &["--max-tool-calls=-1", "--allow-shell"],
        );
        assert_eq!(envelope["status"], "error");
        assert_eq!(envelope["error"]["code"], "invalid-tool-call");
        assert_eq!(requests.len(), 1);
        assert!(!root.path().join("must-not-exist").exists());
        let state = store(root.path()).snapshot().unwrap();
        assert_eq!(state.branches[0].lifecycle, Lifecycle::Failed);
        assert!(state.branches[0].rounds.is_empty());
    }
}

#[path = "provider_tool_name_refusal/placeholder.rs"]
mod placeholder;
