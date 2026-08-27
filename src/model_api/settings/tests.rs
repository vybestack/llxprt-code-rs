use serdes_ai::models::chatgpt_oauth::{CodexReasoningEffort, CodexReasoningSummary};

use super::*;
use crate::session::SessionId;

fn session(value: &str) -> ProviderSessionId {
    ProviderSessionId::from_session_id(&SessionId::parse(value).unwrap()).unwrap()
}

fn reasoning() -> CodexReasoning {
    CodexReasoning {
        effort: CodexReasoningEffort::High,
        summary: CodexReasoningSummary::Auto,
    }
}

#[test]
fn finalization_preserves_wire_fields_and_fixed_policy() {
    let finalized = CodexResponsesSettingsDraft::new(
        "GPT-5.6-Sol".to_string(),
        Some(reasoning()),
        CodexCacheMode::OneHour,
    )
    .finalize(session("Session_01"));

    assert_eq!(finalized.model(), "GPT-5.6-Sol");
    assert_eq!(finalized.endpoint().as_str(), CODEX_ENDPOINT);
    assert_eq!(finalized.reasoning, Some(reasoning()));
    assert_eq!(finalized.text_verbosity, CodexTextVerbosity::Medium);
    assert_eq!(finalized.session_id.as_str(), "Session_01");
    assert_eq!(
        finalized.prompt_cache_key.as_ref().unwrap().as_str(),
        "Session_01"
    );
    assert!(!finalized.store());
    assert_eq!(
        finalized.request_timeout(),
        Duration::from_secs(CODEX_REQUEST_TIMEOUT_SECONDS)
    );
}

#[test]
fn cache_off_omits_only_body_key() {
    let finalized =
        CodexResponsesSettingsDraft::new("gpt-5.6-sol".to_string(), None, CodexCacheMode::Off)
            .finalize(session("cache-off"));

    assert_eq!(finalized.session_id.as_str(), "cache-off");
    assert!(finalized.prompt_cache_key.is_none());
    assert!(finalized.reasoning.is_none());
}

#[test]
fn one_hour_and_twenty_four_hour_have_same_codex_body_key_policy() {
    for mode in [CodexCacheMode::OneHour, CodexCacheMode::TwentyFourHours] {
        let finalized = CodexResponsesSettingsDraft::new("gpt-5.6-sol".to_string(), None, mode)
            .finalize(session("cache-key"));
        assert_eq!(
            finalized.prompt_cache_key.as_ref().unwrap().as_str(),
            "cache-key"
        );
    }
}

#[test]
fn finalized_settings_convert_to_typed_vendored_settings() {
    let settings = CodexResponsesSettingsDraft::new(
        "gpt-5.6-sol".to_string(),
        Some(reasoning()),
        CodexCacheMode::TwentyFourHours,
    )
    .finalize(session("wire-identity"))
    .into_request_settings();

    assert_eq!(settings.reasoning, Some(reasoning()));
    assert_eq!(settings.text_verbosity, CodexTextVerbosity::Medium);
    assert_eq!(settings.session_id.as_str(), "wire-identity");
    assert_eq!(
        settings.prompt_cache_key.as_ref().unwrap().as_str(),
        "wire-identity"
    );
}

#[test]
fn production_endpoint_identity_is_exact() {
    assert_eq!(
        CodexEndpointIdentity::Production.as_str(),
        "https://chatgpt.com/backend-api/codex"
    );
}
