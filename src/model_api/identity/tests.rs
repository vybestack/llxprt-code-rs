use super::*;

fn provider(value: &str) -> ProviderSessionId {
    let session = SessionId::parse(value).unwrap();
    ProviderSessionId::from_session_id(&session).unwrap()
}

#[test]
fn exact_host_boundaries_convert_to_both_vendored_labels() {
    for value in ["a".to_string(), "A0_-".to_string(), "z".repeat(64)] {
        let provider = provider(&value);
        let cache = PromptCacheKey::from_provider_session_id(&provider);
        assert_eq!(provider.to_chatgpt_session_id().as_str(), value);
        assert_eq!(cache.to_chatgpt_prompt_cache_key().as_str(), value);
    }
}

#[test]
fn provider_transition_revalidates_session_invariant() {
    for value in ["", "a b", "a/b", "é", &"z".repeat(65)] {
        let session = SessionId {
            id: value.to_string(),
        };
        assert!(ProviderSessionId::from_session_id(&session).is_err());
    }
}

#[test]
fn cache_key_preserves_exact_session_bytes() {
    let provider = provider("Case_Sensitive-01");
    let cache = PromptCacheKey::from_provider_session_id(&provider);
    assert_eq!(
        cache.to_chatgpt_prompt_cache_key().as_str(),
        "Case_Sensitive-01"
    );
}
