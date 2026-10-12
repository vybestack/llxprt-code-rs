//! Refusal and privacy boundaries for private provider state and current-format records.
use llxprt_code_rs::adapter::{ChatBackend, LlmResult, ToolCall};
use llxprt_code_rs::agent::CodingAgent;
use llxprt_code_rs::session::{SessionId, SessionStore};
use serde_json::json;
use serdes_ai::core::{ModelResponse, ModelResponsePart, ThinkingPart};

fn response(text: &str, opaque: &str) -> LlmResult {
    let mut response = ModelResponse::new();
    response.parts.push(ModelResponsePart::Thinking(
        ThinkingPart::new(text)
            .with_signature(opaque)
            .with_provider_details(
                [("encrypted_content".into(), json!(opaque))]
                    .into_iter()
                    .collect(),
            ),
    ));
    LlmResult::from_response(&response, &["configured-secret".into()])
}

#[test]
fn opaque_state_is_not_a_display_projection_and_required_fields_are_not_legacy_defaults() {
    let result = response("readable", "opaque-fixture");
    let state = result.continuation.unwrap();
    assert!(!format!("{state:?}").contains("opaque-fixture"));
    assert!(serde_json::to_string(&state)
        .unwrap()
        .contains("opaque-fixture"));
    assert_eq!(result.thinking, "readable");
    for legacy in [
        json!({"assistant":"","calls":[]}),
        json!({"thinking":"","assistant":"","calls":[]}),
    ] {
        assert!(serde_json::from_value::<llxprt_code_rs::session::RoundRecord>(legacy).is_err());
    }
    let current = json!({"thinking":"","continuation":null,"assistant":"","calls":[]});
    assert!(serde_json::from_value::<llxprt_code_rs::session::RoundRecord>(current).is_ok());
}

struct Reply(std::sync::Mutex<Option<LlmResult>>);
impl ChatBackend for Reply {
    fn request<'a>(
        &'a self,
        _: &'a [serdes_ai::core::ModelRequest],
        _: &'a [llxprt_code_rs::tools::ToolSpec],
    ) -> llxprt_code_rs::adapter::ModelFuture<'a> {
        Box::pin(async move { Ok(self.0.lock().unwrap().take().unwrap()) })
    }
}

#[test]
fn unsafe_or_oversized_continuation_refuses_before_emission_tools_and_private_persistence() {
    let cases = [
        response("configured-secret", "opaque"),
        response("safe", "configured-secret"),
        response(
            "safe",
            &"x".repeat(llxprt_code_rs::limits::MAX_TURN_ASSISTANT_BYTES + 1),
        ),
    ];
    for (i, mut result) in cases.into_iter().enumerate() {
        let root = tempfile::tempdir().unwrap();
        let store = SessionStore::load_at(
            &SessionId::parse(&format!("unsafe-{i}")).unwrap(),
            root.path(),
        )
        .unwrap();
        let request = store
            .start_request(None, None, "test", root.path())
            .unwrap();
        result.calls.push(ToolCall {
            id: "call".into(),
            name: "write_file".into(),
            args_json: "{\"path\":\"must-not-exist\",\"content\":\"bad\"}".into(),
        });
        let agent = CodingAgent::with_backend(
            Box::new(Reply(std::sync::Mutex::new(Some(result)))),
            root.path().to_owned(),
            false,
        )
        .with_secrets(vec!["configured-secret".into()]);
        let outcome = agent.run(&store, &request).unwrap_err();
        assert_eq!(outcome.key, "model");
        assert!(!root.path().join("must-not-exist").exists());
        let state = store.snapshot().unwrap();
        assert!(state.branches[0].rounds.is_empty());
        assert_eq!(
            state.branches[0].lifecycle,
            llxprt_code_rs::session::Lifecycle::Failed
        );
        let bytes = serde_json::to_string(&state).unwrap();
        assert!(!bytes.contains("configured-secret"));
        assert!(!bytes.contains("opaque-fixture"));
    }
}

#[test]
fn plaintext_reasoning_split_across_blocks_is_rejected_before_it_can_be_retained() {
    let mut raw = ModelResponse::new();
    raw.parts = vec![
        ModelResponsePart::Thinking(ThinkingPart::new("configured-")),
        ModelResponsePart::Thinking(ThinkingPart::new("secret")),
    ];
    let result = LlmResult::from_response(&raw, &["configured-secret".into()]);
    assert_eq!(result.thinking, "[redacted]");
    assert!(result
        .continuation
        .unwrap()
        .validate(&["configured-secret".into()])
        .is_err());
}
