//! Sidecar unit tests: mark writes stat-able JSON, clear removes it, and clearing
//! a missing marker stays a no-op.

use super::*;
use crate::session::{SessionId, SessionStore};

#[test]
fn mark_writes_json_with_tool_and_started() {
    let _config = crate::agent::tests::shared_config_home();
    let store = SessionStore::load(&SessionId::parse("in-flight-mark").unwrap()).unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    mark(
        &store,
        &reserved,
        &ToolCall {
            id: "shell".into(),
            name: "run_shell_command".into(),
            args_json: "{\"command\":\"echo ok\"}".into(),
        },
        2,
        &[],
    )
    .unwrap();
    let path = store.session_dir().join("tool-in-flight.json");
    let parsed: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(parsed["tool"], "run_shell_command");
    assert_eq!(parsed["round"], 2);
    assert_eq!(parsed["turn"], 1);
    assert_eq!(parsed["args"], "{\"command\":\"echo ok\"}");
    let started = parsed["started"].as_u64().unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(started <= now);
    clear(&store, &reserved).unwrap();
    assert!(!path.exists());
}

#[test]
fn clear_on_missing_marker_is_a_no_op() {
    let _config = crate::agent::tests::shared_config_home();
    let store = SessionStore::load(&SessionId::parse("in-flight-clear").unwrap()).unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    assert!(!store.session_dir().join("tool-in-flight.json").exists());
    clear(&store, &reserved).unwrap();
    assert!(!store.session_dir().join("tool-in-flight.json").exists());
}

#[test]
fn arguments_are_scrubbed_before_utf8_bounding() {
    let root = tempfile::tempdir().unwrap();
    let store = SessionStore::load_at(&SessionId::fresh(), root.path()).unwrap();
    let reserved = store.start_request(None, None, "P", root.path()).unwrap();
    let call = ToolCall {
        id: "bounded".into(), name: "run_shell_command".into(),
        args_json: serde_json::json!({"command":format!("echo configured-secret https://u:password@example.com/?secret=hidden {}", "é".repeat(MAX_ARGS_BYTES))}).to_string(),
    };
    mark(&store, &reserved, &call, 93, &["configured-secret".into()]).unwrap();
    let bytes = std::fs::read(store.session_dir().join("tool-in-flight.json")).unwrap();
    let flight: InFlight = serde_json::from_slice(&bytes).unwrap();
    assert!(flight.args.len() <= MAX_ARGS_BYTES);
    assert!(flight.args.ends_with(crate::redact::TRUNCATION_MARKER));
    assert!(flight.args.contains("[redacted]"));
    for secret in ["configured-secret", "password", "hidden"] {
        assert!(!flight.args.contains(secret));
    }
    assert_eq!(flight.round, 93);
    clear(&store, &reserved).unwrap();
}

#[test]
fn failed_publication_prevents_dispatch() {
    let root = tempfile::tempdir().unwrap();
    let store = SessionStore::load_at(&SessionId::fresh(), root.path()).unwrap();
    let reserved = store.start_request(None, None, "P", root.path()).unwrap();
    std::fs::create_dir(store.session_dir().join("tool-in-flight.json")).unwrap();
    let agent = CodingAgent::with_backend(
        Box::new(crate::agent::tests::MockBackend::new(vec![])),
        root.path().into(),
        true,
    );
    let turn = Turn::new(&agent).unwrap();
    let config = turn.tools_config(false).unwrap();
    let call = ToolCall {
        id: "never-run".into(),
        name: "write_file".into(),
        args_json: "{\"path\":\"must-not-exist\",\"content\":\"bad\"}".into(),
    };
    let result = turn.dispatch_recorded(
        &config,
        (&store, &reserved, 1),
        &call,
        serde_json::from_str(&call.args_json).unwrap(),
        1024,
    );
    assert!(matches!(result, Err(ToolCallFailure::Session(_))));
    assert!(!root.path().join("must-not-exist").exists());
}
