//! Sidecar unit tests: mark writes stat-able JSON, clear removes it, and clearing
//! a missing marker stays a no-op.

use super::*;
use crate::session::{SessionId, SessionStore};

#[test]
fn mark_writes_json_with_tool_and_started() {
    let _config = crate::agent::tests::shared_config_home();
    let store = SessionStore::load(&SessionId::parse("in-flight-mark").unwrap()).unwrap();
    mark(&store, "run_shell_command");
    let path = store.session_dir().join("tool-in-flight.json");
    let parsed: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(parsed["tool"], "run_shell_command");
    let started = parsed["started"].as_u64().unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(started <= now);
    clear(&store);
    assert!(!path.exists());
}

#[test]
fn clear_on_missing_marker_is_a_no_op() {
    let _config = crate::agent::tests::shared_config_home();
    let store = SessionStore::load(&SessionId::parse("in-flight-clear").unwrap()).unwrap();
    assert!(!store.session_dir().join("tool-in-flight.json").exists());
    clear(&store);
    assert!(!store.session_dir().join("tool-in-flight.json").exists());
}
