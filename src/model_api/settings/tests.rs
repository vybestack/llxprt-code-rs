use super::*;

#[test]
fn codex_profile_effort_reaches_serialized_reasoning() {
    let base: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/task-profile/astra-headless.json"
    ))
    .unwrap();
    for effort in ["low", "medium", "high"] {
        let mut value = base.clone();
        value["ephemeralSettings"]["reasoning.effort"] = serde_json::json!(effort);
        let profile = crate::profile::parse_profile_value(&value, "astra").unwrap();
        let resolved = crate::model_api::interpret::ResolvedProfile::interpret(&profile).unwrap();
        let reasoning = resolved.codex.unwrap().responses_reasoning().unwrap();
        assert_eq!(
            serde_json::to_value(reasoning).unwrap(),
            serde_json::json!({
                "effort": effort, "summary": "auto"
            })
        );
    }
    for effort in [
        serde_json::json!("invalid"),
        serde_json::json!(42),
        serde_json::json!(null),
    ] {
        let mut value = base.clone();
        value["ephemeralSettings"]["reasoning.effort"] = effort;
        assert!(crate::profile::parse_profile_value(&value, "astra").is_err());
    }
    let mut disabled = base;
    let settings = disabled["ephemeralSettings"].as_object_mut().unwrap();
    settings.insert("reasoning.enabled".into(), serde_json::json!(false));
    settings.remove("reasoning.effort");
    settings.remove("reasoning.summary");
    let profile = crate::profile::parse_profile_value(&disabled, "astra").unwrap();
    let resolved = crate::model_api::interpret::ResolvedProfile::interpret(&profile).unwrap();
    assert!(resolved.codex.unwrap().responses_reasoning().is_none());
}

#[test]
fn production_endpoint_identity_is_exact() {
    let draft =
        CodexResponsesSettingsDraft::new("gpt-5.6-sol".to_string(), Some("high".to_string()));
    assert_eq!(
        draft.endpoint().responses_url(),
        "https://chatgpt.com/backend-api/codex/responses"
    );
    assert_eq!(draft.model(), "gpt-5.6-sol");
}

#[test]
fn enabled_reasoning_maps_to_tested_responses_shape() {
    let draft =
        CodexResponsesSettingsDraft::new("gpt-5.6-sol".to_string(), Some("high".to_string()));
    let reasoning = draft.responses_reasoning().unwrap();
    assert_eq!(reasoning.effort.as_deref(), Some("high"));
    assert_eq!(reasoning.summary, Some(serde_json::json!("auto")));
}

#[test]
fn disabled_reasoning_omits_responses_reasoning() {
    let draft = CodexResponsesSettingsDraft::new("gpt-5.6-sol".to_string(), None);
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
