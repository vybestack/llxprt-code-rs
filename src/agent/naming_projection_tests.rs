//! PR245/main integration: charged naming failures retain main's live/durable seam.
use super::naming_recovery_tests::{call, reply, ObservingBackend};
use super::tests::{shared_config_home, MockBackend};
use super::*;
use crate::session::{Lifecycle, SessionId};

#[test]
fn mixed_naming_and_bulk_read_keep_live_bytes_but_reopen_digest_only() {
    let _config = shared_config_home();
    let cwd = tempfile::tempdir().unwrap();
    let body = "bulk-live-evidence\n".repeat(128);
    std::fs::write(cwd.path().join("bulk.txt"), &body).unwrap();
    let id = SessionId::parse("naming-projection").unwrap();
    let store = SessionStore::load(&id).unwrap();
    let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    let mut read = call("read", "read_file");
    read.args_json = r#"{"path":"bulk.txt"}"#.into();
    let calls = vec![call("unknown", "run_socket_command"), read];
    let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let agent = CodingAgent::with_backend(
        Box::new(ObservingBackend {
            script: MockBackend::new(vec![reply(calls.clone()), reply(vec![])]),
            requests: captured.clone(),
        }),
        cwd.path().into(),
        false,
    );
    assert_eq!(agent.run(&store, &reserved).unwrap().tool_count, 2);
    let batches = captured.lock().unwrap();
    let returns = batches[1]
        .iter()
        .flat_map(|r| &r.parts)
        .filter_map(|p| match p {
            serdes_ai::core::ModelRequestPart::ToolReturn(r) => Some(r),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(returns.len(), 2);
    for (returned, call) in returns.iter().zip(&calls) {
        assert_eq!(returned.tool_name, call.name);
        assert_eq!(returned.tool_call_id.as_deref(), Some(call.id.as_str()));
    }
    assert!(returns[0]
        .content
        .to_string_content()
        .contains("available:"));
    let live = returns[1].content.to_string_content();
    assert_eq!(
        live,
        format!("[0..{} of {} bytes]\n{body}", body.len(), body.len())
    );
    assert!(!live.contains("CTXDIGEST"));
    drop(batches);
    let state = SessionStore::load(&id).unwrap().snapshot().unwrap();
    let records = &state.branches[0].rounds[0].calls;
    assert!(!records[0].ok);
    assert!(!records[0].refused);
    assert!(records[1].ok);
    assert!(records[1].result.contains("CTXDIGEST"));
    assert!(records[1].result.contains(&format!("bytes={}", live.len())));
    assert!(records.iter().all(|r| r.result_live.is_empty()));
    let durable = serde_json::to_string(&state).unwrap();
    assert!(!durable.contains("result_live"));
    assert!(!durable.contains("bulk-live-evidence"));
}

#[test]
fn output_exhaustion_keeps_charged_naming_prefix_and_never_executes_suffix() {
    let _config = shared_config_home();
    let cwd = tempfile::tempdir().unwrap();
    let id = SessionId::parse("naming-executed-prefix").unwrap();
    let store = SessionStore::load(&id).unwrap();
    let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    let mut write = call("write", "write_file");
    write.args_json = r#"{"path":"must-not-exist","content":"bad"}"#.into();
    let agent = CodingAgent::with_backend(
        Box::new(MockBackend::new(vec![reply(vec![
            call("unknown", "run_socket_command"),
            write,
        ])])),
        cwd.path().into(),
        false,
    )
    .with_output_caps(OutputCaps {
        tool: 32,
        turn: 32,
        ..OutputCaps::default()
    });
    assert_eq!(agent.run(&store, &reserved).unwrap_err().key, "limit");
    assert_eq!(agent.model_calls(), 1);
    assert!(!cwd.path().join("must-not-exist").exists());
    let state = SessionStore::load(&id).unwrap().snapshot().unwrap();
    let branch = &state.branches[0];
    assert_eq!(branch.lifecycle, Lifecycle::Failed);
    assert_eq!(branch.rounds.len(), 1);
    assert_eq!(branch.rounds[0].calls.len(), 1);
    let record = &branch.rounds[0].calls[0];
    assert_eq!(record.id, "unknown");
    assert_eq!((record.ok, record.refused), (false, false));
    assert_eq!(record.result.len(), 32);
    assert!(record.result_live.is_empty());
    assert!(!store.session_dir().join("tool-in-flight.json").exists());
}
