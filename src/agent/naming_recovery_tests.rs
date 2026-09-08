//! Issue 206: naming failures share the ordered, charged tool-result path.
use super::tests::{shared_config_home, MockBackend};
use super::*;
use crate::adapter::LlmUsage;
use crate::session::SessionId;
use serdes_ai::core::FinishReason;

fn call(id: &str, name: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        args_json: "{}".into(),
    }
}

fn reply(calls: Vec<ToolCall>) -> LlmResult {
    let done = calls.is_empty();
    LlmResult {
        text: if done { "done".into() } else { String::new() },
        calls,
        finish_reason: Some(if done {
            FinishReason::Stop
        } else {
            FinishReason::ToolCall
        }),
        usage: LlmUsage::default(),
    }
}

#[test]
fn mixed_names_use_original_order_and_budget_prefix() {
    let cwd = tempfile::tempdir().unwrap();
    let _config = shared_config_home();
    let store = SessionStore::load(&SessionId::parse("naming-mixed").unwrap()).unwrap();
    let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    let mut write = call("write", "write_file");
    write.args_json = r#"{"path":"must-not-exist","content":"bad"}"#.into();
    let calls = vec![
        call("unknown", "run_socket_command"),
        call("valid", "list_directory"),
        write,
    ];
    let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let agent = CodingAgent::with_backend(
        Box::new(ObservingBackend {
            script: MockBackend::new(vec![reply(calls.clone()), reply(vec![])]),
            requests: requests.clone(),
        }),
        cwd.path().into(),
        false,
    )
    .with_max_tool_calls(Some(2));
    let run = agent.run(&store, &reserved).unwrap();
    assert_eq!(run.tool_count, 2);
    assert!(run.budget_exhausted);
    assert!(!cwd.path().join("must-not-exist").exists());
    let state = store.snapshot().unwrap();
    let recorded = &state.branches[0].rounds[0].calls;
    assert_eq!(
        recorded
            .iter()
            .map(|c| (&c.id, &c.name, &c.args))
            .collect::<Vec<_>>(),
        calls
            .iter()
            .map(|c| (&c.id, &c.name, &c.args_json))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        recorded.iter().map(|c| c.refused).collect::<Vec<_>>(),
        vec![false, false, true]
    );
    assert!(!recorded[0].ok);
    assert!(recorded[0].result.contains("available:"));
    let requests = requests.lock().unwrap();
    let start = requests[0].len();
    assert_eq!(
        semantic_requests(&requests[1][start..start + calls.len() + 1]),
        semantic_requests(&persisted_round_requests(&state.branches[0].rounds[0]))
    );
}

#[test]
fn repeated_unknown_names_cannot_bypass_call_budget() {
    let cwd = tempfile::tempdir().unwrap();
    let _config = shared_config_home();
    let store = SessionStore::load(&SessionId::parse("naming-repeat").unwrap()).unwrap();
    let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    let agent = CodingAgent::with_backend(
        Box::new(MockBackend::new(vec![
            reply(vec![call("one", "run_socket_command")]),
            reply(vec![
                call("two", "run_socket_command"),
                call("three", "run_socket_command"),
            ]),
            reply(vec![]),
        ])),
        cwd.path().into(),
        false,
    )
    .with_max_tool_calls(Some(2));
    let run = agent.run(&store, &reserved).unwrap();
    assert_eq!(run.tool_count, 2);
    assert!(run.budget_exhausted);
    assert_eq!(agent.model_calls(), 3);
    let state = store.snapshot().unwrap();
    let calls = state.branches[0]
        .rounds
        .iter()
        .flat_map(|r| &r.calls)
        .collect::<Vec<_>>();
    assert_eq!(calls.iter().filter(|c| !c.refused).count(), run.tool_count);
    assert!(calls[2].refused);
    assert!(calls[2].result.contains("budget exhausted"));
}

struct ObservingBackend {
    script: MockBackend,
    requests: std::sync::Arc<std::sync::Mutex<Vec<Vec<serdes_ai::core::ModelRequest>>>>,
}

impl ChatBackend for ObservingBackend {
    fn request(
        &self,
        requests: &[serdes_ai::core::ModelRequest],
        tools: &[crate::tools::ToolSpec],
    ) -> Result<LlmResult, String> {
        self.requests.lock().unwrap().push(requests.to_vec());
        self.script.request(requests, tools)
    }
    fn request_calls(&self) -> usize {
        self.script.request_calls()
    }
}

#[test]
fn socket_name_corrects_to_shell_in_same_turn_with_matching_history() {
    let cwd = tempfile::tempdir().unwrap();
    let _config = shared_config_home();
    let id = SessionId::parse("naming-correction").unwrap();
    let store = SessionStore::load(&id).unwrap();
    let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    let mut bad = call("bad", "run_socket_command");
    bad.args_json = r#"{"command":"printf corrected > corrected","timeout_seconds":1}"#.into();
    let good = ToolCall {
        id: "good".into(),
        name: "run_shell_command".into(),
        ..bad.clone()
    };
    let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let agent = CodingAgent::with_backend(
        Box::new(ObservingBackend {
            script: MockBackend::new(vec![reply(vec![bad]), reply(vec![good]), reply(vec![])]),
            requests: requests.clone(),
        }),
        cwd.path().into(),
        true,
    )
    .with_max_tool_calls(Some(3));
    let run = agent.run(&store, &reserved).unwrap();
    assert_eq!((run.turn, run.attempt, run.tool_count), (1, 1, 2));
    assert_eq!(
        std::fs::read_to_string(cwd.path().join("corrected")).unwrap(),
        "corrected"
    );
    let state = SessionStore::load(&id).unwrap().snapshot().unwrap();
    let rounds = &state.branches[0].rounds;
    assert!(!rounds[0].calls[0].ok);
    assert!(rounds[1].calls[0].ok);
    let recorded = rounds[..2]
        .iter()
        .flat_map(persisted_round_requests)
        .collect::<Vec<_>>();
    let requests = requests.lock().unwrap();
    let live = &requests[2][requests[0].len()..];
    assert_eq!(semantic_requests(live), semantic_requests(&recorded));
    let replay = store.start_request(Some(1), None, "P", cwd.path()).unwrap();
    assert!(replay.replay);
    assert_eq!(agent.run(&store, &replay).unwrap().tool_count, 2);
    assert_eq!(agent.model_calls(), 3);
}

#[test]
fn disabled_shell_never_executes_and_failure_is_bounded() {
    let cwd = tempfile::tempdir().unwrap();
    let _config = shared_config_home();
    let store = SessionStore::load(&SessionId::parse("naming-disabled").unwrap()).unwrap();
    let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    let mut shell = call("disabled", "run_shell_command");
    shell.args_json = r#"{"command":"touch must-not-exist","timeout_seconds":1}"#.into();
    let agent = CodingAgent::with_backend(
        Box::new(MockBackend::new(vec![reply(vec![shell]), reply(vec![])])),
        cwd.path().into(),
        false,
    )
    .with_output_caps(OutputCaps {
        tool: 64,
        ..OutputCaps::default()
    });
    let run = agent.run(&store, &reserved).unwrap();
    assert_eq!(run.tool_count, 1);
    assert!(!cwd.path().join("must-not-exist").exists());
    let state = store.snapshot().unwrap();
    let failure = &state.branches[0].rounds[0].calls[0];
    assert_eq!((failure.ok, failure.refused), (false, false));
    assert!(failure.result.len() <= 64);
    assert!(!failure.result.contains("touch"));
}

#[test]
fn malformed_unknown_args_abort_entire_batch_before_side_effects() {
    for (index, args) in ["not json", "[]", "null"].into_iter().enumerate() {
        let cwd = tempfile::tempdir().unwrap();
        let _config = shared_config_home();
        let store =
            SessionStore::load(&SessionId::parse(&format!("naming-malformed-{index}")).unwrap())
                .unwrap();
        let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
        let mut write = call("write", "write_file");
        write.args_json = r#"{"path":"must-not-exist","content":"bad"}"#.into();
        let mut bad = call("bad", "run_socket_command");
        bad.args_json = args.into();
        let agent = CodingAgent::with_backend(
            Box::new(MockBackend::new(vec![reply(vec![write, bad])])),
            cwd.path().into(),
            false,
        );
        assert_eq!(
            agent.run(&store, &reserved).unwrap_err().key,
            "invalid-tool-call"
        );
        assert_eq!(agent.model_calls(), 1);
        assert!(!cwd.path().join("must-not-exist").exists());
        assert!(store.snapshot().unwrap().branches[0].rounds.is_empty());
    }
}

#[test]
fn naming_failures_obey_existing_round_and_time_limits() {
    let cwd = tempfile::tempdir().unwrap();
    let _config = shared_config_home();
    let store = SessionStore::load(&SessionId::parse("naming-time").unwrap()).unwrap();
    let mut reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    let mut agent = CodingAgent::with_backend(
        Box::new(MockBackend::new(vec![reply(vec![call(
            "one",
            "run_socket_command",
        )])])),
        cwd.path().into(),
        false,
    );
    let tools = crate::tools::tool_specs(false);
    let config = agent.tools_config(false).unwrap();
    let requests = agent.materialize_requests(&reserved);
    let mut attempt = agent
        .begin_attempt(&store, &mut reserved, requests, &tools)
        .unwrap();
    agent
        .execute_tool_round(&store, &reserved, &config, &mut attempt)
        .unwrap();
    attempt.current = reply(vec![call("two", "run_socket_command")]);
    // Inject elapsed time, not a sleep or an in-flight deadline implementation.
    agent.turn_time_budget = Some(std::time::Duration::from_secs(1));
    attempt.started = std::time::Instant::now() - std::time::Duration::from_secs(2);
    assert_eq!(
        agent
            .execute_tool_round(&store, &reserved, &config, &mut attempt)
            .unwrap_err()
            .key,
        "turn-time-exhausted"
    );
    assert_eq!(attempt.usage.total_calls, 1);
    assert_eq!(store.snapshot().unwrap().branches[0].rounds.len(), 1);

    let store = SessionStore::load(&SessionId::parse("naming-rounds").unwrap()).unwrap();
    let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    let agent = CodingAgent::with_backend(
        Box::new(MockBackend::new(vec![
            reply(vec![call("one", "run_socket_command")]),
            reply(vec![call("two", "run_socket_command")]),
        ])),
        cwd.path().into(),
        false,
    )
    .with_max_rounds(1);
    assert_eq!(agent.run(&store, &reserved).unwrap_err().key, "turn-budget");
    assert_eq!(store.snapshot().unwrap().branches[0].rounds.len(), 1);
}

// Timestamps belong to each materialization, not the persisted call/result identity.
fn semantic_requests(requests: &[serdes_ai::core::ModelRequest]) -> JsonValue {
    let mut value = serde_json::to_value(requests).unwrap();
    for request in value.as_array_mut().unwrap() {
        for part in request["parts"].as_array_mut().unwrap() {
            part.as_object_mut().unwrap().remove("timestamp");
        }
    }
    value
}

struct DeniedBackend;
impl ChatBackend for DeniedBackend {
    fn request(
        &self,
        _: &[serdes_ai::core::ModelRequest],
        _: &[crate::tools::ToolSpec],
    ) -> Result<LlmResult, String> {
        Err("HTTP 403 authorization denied".into())
    }
    fn request_calls(&self) -> usize {
        1
    }
}

#[test]
fn authorization_denial_is_not_a_correctable_naming_failure() {
    let cwd = tempfile::tempdir().unwrap();
    let _config = shared_config_home();
    let store = SessionStore::load(&SessionId::parse("naming-auth").unwrap()).unwrap();
    let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    let agent = CodingAgent::with_backend(Box::new(DeniedBackend), cwd.path().into(), false);
    let error = agent.run(&store, &reserved).unwrap_err();
    assert_eq!(error.key, "model");
    assert!(error.message.contains("authorization denied"));
    let state = store.snapshot().unwrap();
    assert_eq!(
        state.branches[0].lifecycle,
        crate::session::Lifecycle::Failed
    );
    assert!(state.branches[0].rounds.is_empty());
}

#[test]
fn unsafe_or_oversized_names_and_secret_args_stay_fail_closed() {
    let secret = "configured-secret";
    for (index, (name, args)) in [
        ("run_shell_command;touch bypass".into(), "{}".into()),
        ("<invoke name=run_shell_command>".into(), "{}".into()),
        ("x".repeat(MAX_TOOL_NAME_BYTES + 1), "{}".into()),
        (
            "run_socket_command".into(),
            format!(r#"{{"command":"{secret}"}}"#),
        ),
        (secret.into(), "{}".into()),
    ]
    .into_iter()
    .enumerate()
    {
        let cwd = tempfile::tempdir().unwrap();
        let _config = shared_config_home();
        let store =
            SessionStore::load(&SessionId::parse(&format!("naming-unsafe-{index}")).unwrap())
                .unwrap();
        let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
        let bad = ToolCall {
            id: "bad".into(),
            name,
            args_json: args,
        };
        let agent = CodingAgent::with_backend(
            Box::new(MockBackend::new(vec![reply(vec![bad])])),
            cwd.path().into(),
            true,
        )
        .with_secrets(vec![secret.into()]);
        let error = agent.run(&store, &reserved).unwrap_err();
        assert_eq!(error.key, "model");
        assert!(!error.message.contains(secret));
        assert_eq!(agent.model_calls(), 1);
        let state = store.snapshot().unwrap();
        assert!(state.branches[0].rounds.is_empty());
        assert!(!serde_json::to_string(&state).unwrap().contains(secret));
        assert!(!cwd.path().join("bypass").exists());
    }
}

#[test]
fn naming_failure_does_not_echo_args() {
    let name = "run_socket_command";
    let cwd = tempfile::tempdir().unwrap();
    let _config = shared_config_home();
    let store = SessionStore::load(&SessionId::parse("naming-scrub").unwrap()).unwrap();
    let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    let mut bad = call("bad", name);
    bad.args_json = r#"{"command":"sensitive-argument-not-for-error"}"#.into();
    let agent = CodingAgent::with_backend(
        Box::new(MockBackend::new(vec![reply(vec![bad]), reply(vec![])])),
        cwd.path().into(),
        false,
    );
    agent.run(&store, &reserved).unwrap();
    let state = store.snapshot().unwrap();
    let result = &state.branches[0].rounds[0].calls[0].result;
    assert!(result.contains(name));
    assert!(!result.contains("sensitive-argument"));
    assert!(result.len() <= crate::redact::MAX_ERROR_TEXT_BYTES);
}

#[test]
fn repeated_names_exhaust_output_cap_before_next_failure() {
    let cwd = tempfile::tempdir().unwrap();
    let _config = shared_config_home();
    let store = SessionStore::load(&SessionId::parse("naming-output").unwrap()).unwrap();
    let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
    let agent = CodingAgent::with_backend(
        Box::new(MockBackend::new(vec![
            reply(vec![call("one", "run_socket_command")]),
            reply(vec![call("two", "run_socket_command")]),
        ])),
        cwd.path().into(),
        false,
    )
    .with_output_caps(OutputCaps {
        tool: 32,
        turn: 32,
        ..OutputCaps::default()
    });
    assert_eq!(agent.run(&store, &reserved).unwrap_err().key, "limit");
    let state = store.snapshot().unwrap();
    assert_eq!(state.branches[0].rounds.len(), 1);
    assert!(state.branches[0].rounds[0].calls[0].result.len() <= 32);
}
