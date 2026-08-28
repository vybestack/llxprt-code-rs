use super::*;

#[test]
fn production_endpoint_identity_is_exact() {
    let draft = CodexResponsesSettingsDraft::new("gpt-5.6-sol".to_string(), true);
    assert_eq!(
        draft.endpoint().websocket_url(),
        "wss://chatgpt.com/backend-api/codex/responses"
    );
    assert_eq!(draft.model(), "gpt-5.6-sol");
}

#[test]
fn enabled_reasoning_maps_to_tested_responses_shape() {
    let draft = CodexResponsesSettingsDraft::new("gpt-5.6-sol".to_string(), true);
    let reasoning = draft.responses_reasoning().unwrap();
    assert_eq!(reasoning.effort.as_deref(), Some("high"));
    assert_eq!(reasoning.summary, Some(serde_json::json!("auto")));
}

#[test]
fn disabled_reasoning_omits_responses_reasoning() {
    let draft = CodexResponsesSettingsDraft::new("gpt-5.6-sol".to_string(), false);
    assert!(draft.responses_reasoning().is_none());
}
