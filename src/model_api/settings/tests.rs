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

#[test]
fn openai_responses_cache_settings_are_session_bound_and_stateless() {
    let session = crate::session::SessionId::parse("session_123").unwrap();
    let cached = OpenAiResponsesSettingsDraft {
        reasoning_effort: None,
        reasoning_summary: None,
        text_verbosity: None,
        prompt_caching: PromptCaching::Cached,
    }
    .finalize(&session);
    assert_eq!(cached.prompt_cache_key.as_deref(), Some("session_123"));
    assert_eq!(
        cached.prompt_cache_retention,
        Some(serdes_ai::models::openai::PromptCacheRetention::Hours24)
    );
    assert!(cached.previous_response_id.is_none());
    assert!(!cached.send_reasoning_ids);

    let off = OpenAiResponsesSettingsDraft {
        reasoning_effort: None,
        reasoning_summary: None,
        text_verbosity: None,
        prompt_caching: PromptCaching::Off,
    }
    .finalize(&session);
    assert!(off.prompt_cache_key.is_none());
    assert!(off.prompt_cache_retention.is_none());
}
