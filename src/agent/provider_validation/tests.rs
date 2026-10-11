//! Issue 310: bounded whole-reply refusal, not name normalization or partial dispatch.
use super::*;
use crate::agent::naming_recovery_tests::{call, reply, ObservingBackend};
use crate::agent::tests::{shared_config_home, MockBackend};
use crate::agent::{CodingAgent, Turn, MAX_TOOL_CALL_ID_BYTES, MAX_TOOL_NAME_BYTES};
use crate::session::{Lifecycle, SessionId, SessionStore};
use serdes_ai::core::FinishReason;
use std::sync::{Arc, Mutex};

#[test]
fn byte_boundary_and_plain_prose_are_not_name_parsing() {
    assert!(validate(&reply(vec![call("ok", &"x".repeat(64))]), &[]).is_ok());
    for name in [
        "x".repeat(65),
        "界".repeat(22),
        String::new(),
        "bad/name".into(),
    ] {
        let Err(RoundFailure::ToolCallRefusal(message)) =
            validate(&reply(vec![call("bad", &name)]), &[])
        else {
            panic!("invalid field was not refused");
        };
        assert!(message.contains(&format!("bytes=0..{}", name.len())));
        assert!(message.contains("tool_calls[0].name"));
    }
    let mut prose = reply(vec![]);
    prose.text = format!("Ordinary explanation mentioning {}", "long_name".repeat(30));
    assert!(validate(&prose, &[]).is_ok());
}

#[test]
fn diagnostics_scrub_before_bounding_and_escape_controls() {
    let name = format!(
        "bad\n\r\t\u{1b} https://user:password@host/path?token=private {}",
        "界".repeat(400)
    );
    let message = name_diagnostic(3, &name, "invalid name", &[]);
    assert!(!message.contains("password"));
    assert!(!message.contains("private"));
    assert!(!message.chars().any(char::is_control));
    assert!(message.contains("\\n\\r\\t\\u001b"));
    assert!(message.contains("[truncated]"));
    assert!(message.len() < 1024);
    let name = format!("{}edge-secret", "x".repeat(250));
    let message = name_diagnostic(0, &name, "invalid name", &["edge-secret".into()]);
    assert!(!message.contains("edge-"));
}

#[test]
fn all_secret_and_id_checks_precede_name_diagnostics() {
    for field in 0..4 {
        let mut result = reply(vec![
            call("bad", &"x".repeat(65)),
            call("next", "read_file"),
        ]);
        match field {
            0 => result.text = "configured-secret".into(),
            1 => result.calls[1].id = "configured-secret".into(),
            2 => result.calls[1].name = "configured-secret".into(),
            _ => result.calls[1].args_json = r#"{"path":"configured-secret"}"#.into(),
        }
        let Err(RoundFailure::Model(message)) = validate(&result, &["configured-secret".into()])
        else {
            panic!("secret reply must not be retried or excerpted");
        };
        assert_eq!(message, "model response contained a configured secret");
    }
    let result = reply(vec![
        call("bad", &"x".repeat(65)),
        call(&"i".repeat(MAX_TOOL_CALL_ID_BYTES + 1), "read_file"),
    ]);
    assert!(matches!(
        validate(&result, &[]),
        Err(RoundFailure::Model(_))
    ));
}

#[test]
fn malformed_ids_arguments_and_finish_reasons_are_not_retried() {
    for field in 0..4 {
        let mut result = reply(vec![
            call("bad", &"x".repeat(65)),
            call("next", "read_file"),
        ]);
        match field {
            0 => result.calls[1].id.clear(),
            1 => result.calls[1].id = "bad".into(),
            2 => result.calls[1].args_json = "[]".into(),
            _ => result.calls[1].args_json = "{broken".into(),
        }
        assert!(matches!(
            validate(&result, &[]),
            Err(RoundFailure::InvalidToolCall(_))
        ));
    }
    for finish in [
        None,
        Some(FinishReason::Stop),
        Some(FinishReason::Length),
        Some(FinishReason::ContentFilter),
    ] {
        let mut result = reply(vec![call("bad", &"x".repeat(65))]);
        result.finish_reason = finish;
        assert!(matches!(
            validate(&result, &[]),
            Err(RoundFailure::FinishReason(_))
        ));
    }
}

#[test]
fn corrective_reply_never_executes_or_replays_any_of_refused_batch() {
    let cwd = tempfile::tempdir().unwrap();
    shared_config_home();
    let store = SessionStore::load(&SessionId::parse("name-refusal-corrected").unwrap()).unwrap();
    let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    let mut write = call("write", "write_file");
    write.args_json = r#"{"path":"must-not-exist","content":"sensitive-argument"}"#.into();
    let bad = call("bad", &"x".repeat(MAX_TOOL_NAME_BYTES + 1));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = CodingAgent::with_backend(
        Box::new(ObservingBackend {
            script: MockBackend::new(vec![reply(vec![write, bad]), reply(vec![])]),
            requests: requests.clone(),
        }),
        cwd.path().into(),
        true,
    )
    .with_max_tool_calls(Some(0));
    let run = agent.run(&store, &reserved).unwrap();
    assert_eq!(run.tool_count, 0);
    assert_eq!(agent.model_calls(), 2);
    assert!(!cwd.path().join("must-not-exist").exists());
    let requests = requests.lock().unwrap();
    assert_eq!(requests[1].len(), requests[0].len() + 1);
    let correction = serde_json::to_string(&requests[1]).unwrap();
    assert!(!correction.contains("sensitive-argument"));
    assert!(!correction.contains(&"x".repeat(65)));
    let state = store.snapshot().unwrap();
    assert_eq!(state.branches[0].lifecycle, Lifecycle::Completed);
    assert!(state.branches[0]
        .rounds
        .iter()
        .all(|round| round.calls.is_empty()));
}

#[test]
fn repeated_refusal_keeps_completed_round_and_allows_explicit_retry() {
    let cwd = tempfile::tempdir().unwrap();
    shared_config_home();
    let id = SessionId::parse("name-refusal-after-work").unwrap();
    let store = SessionStore::load(&id).unwrap();
    let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    let mut read = call("read", "list_directory");
    read.args_json = r#"{"path":"."}"#.into();
    let bad = || reply(vec![call("bad", &"x".repeat(65))]);
    let agent = CodingAgent::with_backend(
        Box::new(MockBackend::new(vec![reply(vec![read]), bad(), bad()])),
        cwd.path().into(),
        false,
    );
    let error = agent.run(&store, &reserved).unwrap_err();
    assert_eq!(error.key, crate::agent::MALFORMED_TOOL_CALL_KEY);
    assert_eq!(
        error.terminal_outcome,
        Some(crate::agent::MALFORMED_TOOL_CALL_KEY)
    );
    assert_eq!(error.code, crate::envelope::Code::Model);
    assert_eq!(agent.model_calls(), 3);
    assert!(error.message.contains("one corrective reissue exhausted"));
    drop(store);
    let store = SessionStore::load(&id).unwrap();
    let state = store.snapshot().unwrap();
    assert_eq!(state.branches[0].lifecycle, Lifecycle::Failed);
    assert_eq!(state.branches[0].rounds.len(), 1);
    let retry = store.start_request(Some(1), None, "P", cwd.path()).unwrap();
    assert!(retry.retry);
    let agent = CodingAgent::with_backend(
        Box::new(MockBackend::new(vec![reply(vec![])])),
        cwd.path().into(),
        false,
    );
    assert!(agent.run(&store, &retry).is_ok());
}

#[test]
fn correction_obeys_request_budget_and_original_turn_deadline() {
    let cwd = tempfile::tempdir().unwrap();
    let agent = CodingAgent::with_backend(
        Box::new(MockBackend::new(vec![reply(vec![call(
            "bad",
            &"x".repeat(65),
        )])])),
        cwd.path().into(),
        false,
    )
    .with_context_limit(Some(1));
    let turn = Turn::new(&agent).unwrap();
    let Err(RoundFailure::ToolCallRefusal(message)) = turn.round(&[], &[]) else {
        panic!("correction ignored the request budget");
    };
    assert!(message.contains("request budget"));
    assert_eq!(agent.model_calls(), 1);

    struct Expiring(std::sync::atomic::AtomicUsize);
    impl crate::adapter::ChatBackend for Expiring {
        fn request<'a>(
            &'a self,
            _: &'a [serdes_ai::core::ModelRequest],
            _: &'a [crate::tools::ToolSpec],
        ) -> crate::adapter::ModelFuture<'a> {
            Box::pin(async move {
                let index = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if index == 0 {
                    Ok(reply(vec![call("bad", &"x".repeat(65))]))
                } else {
                    std::future::pending().await
                }
            })
        }
    }
    let agent = CodingAgent::with_backend(
        Box::new(Expiring(std::sync::atomic::AtomicUsize::new(0))),
        cwd.path().into(),
        false,
    )
    .with_turn_time(Some(std::time::Duration::from_millis(30)));
    let turn = Turn::new(&agent).unwrap();
    assert!(matches!(turn.round(&[], &[]), Err(RoundFailure::TurnTime)));
    assert!(turn.started.elapsed() < std::time::Duration::from_secs(1));
}
