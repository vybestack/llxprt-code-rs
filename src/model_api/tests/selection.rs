use serde_json::json;

use crate::model_api::target::{
    resolve_model_target, ModelApi, ModelTarget, ProviderId, TransportKind,
};

fn resolve(provider: &str, settings: serde_json::Value) -> Result<ModelTarget, String> {
    let provider = ProviderId::parse(&json!(provider), "selection")?;
    resolve_model_target(provider, Some(&settings), "selection")
}

#[test]
fn provider_spellings_are_exact() {
    for (raw, expected) in [
        ("openai", ProviderId::OpenAi),
        ("openai-responses", ProviderId::OpenAiResponses),
        ("openaivercel", ProviderId::OpenAiVercel),
        ("openai-compatible", ProviderId::OpenAiCompatible),
        ("anthropic", ProviderId::Anthropic),
        ("codex", ProviderId::Codex),
    ] {
        assert_eq!(
            ProviderId::parse(&json!(raw), "selection").unwrap(),
            expected
        );
    }
    for unsupported in ["openai-vercel", "OpenAI", " openai", "bedrock", ""] {
        assert!(
            ProviderId::parse(&json!(unsupported), "selection").is_err(),
            "{unsupported:?}"
        );
    }
    assert!(ProviderId::parse(&json!(1), "selection").is_err());
}

#[test]
fn selectors_follow_precedence_without_lower_priority_disagreement() {
    let target = resolve(
        "openai",
        json!({
            "apiMode": "responses",
            "responsesMode": "chat",
            "responses-mode": "chat"
        }),
    )
    .unwrap();
    assert_eq!(target.api, ModelApi::Responses);

    let target = resolve(
        "openai",
        json!({"responsesMode": "responses", "responses-mode": "chat"}),
    )
    .unwrap();
    assert_eq!(target.api, ModelApi::Responses);

    let target = resolve("openai", json!({"responses-mode": "responses"})).unwrap();
    assert_eq!(target.api, ModelApi::Responses);
}

#[test]
fn every_explicit_selector_is_validated_before_precedence() {
    for settings in [
        json!({"apiMode": "responses", "responsesMode": ""}),
        json!({"apiMode": "responses", "responses-mode": "Responses"}),
        json!({"apiMode": "responses", "responsesMode": false}),
        json!({"apiMode": " responses"}),
    ] {
        assert!(resolve("openai", settings).is_err());
    }
}

#[test]
fn response_metadata_does_not_select_an_api() {
    let chat = resolve("openai", json!({"openaiResponsesEnabled": true})).unwrap();
    assert_eq!(chat.api, ModelApi::ChatCompletions);

    let responses = resolve(
        "openai",
        json!({"apiMode": "responses", "openaiResponsesEnabled": false}),
    )
    .unwrap();
    assert_eq!(responses.api, ModelApi::Responses);

    assert!(resolve("openai", json!({"openaiResponsesEnabled": "true"})).is_err());
}

#[test]
fn provider_defaults_and_api_compatibility_are_typed() {
    for provider in ["openai", "openaivercel", "openai-compatible"] {
        let target = resolve(provider, json!({})).unwrap();
        assert_eq!(target.api, ModelApi::ChatCompletions, "{provider}");
    }
    let responses = resolve("openai-responses", json!({})).unwrap();
    assert_eq!(responses.api, ModelApi::Responses);
    assert_eq!(responses.transport, TransportKind::Http);

    let anthropic = resolve("anthropic", json!({})).unwrap();
    assert_eq!(anthropic.api, ModelApi::AnthropicMessages);
    assert_eq!(anthropic.transport, TransportKind::Http);

    let codex = resolve("codex", json!({})).unwrap();
    assert_eq!(codex.api, ModelApi::Responses);
    assert_eq!(codex.transport, TransportKind::Http);

    for api_mode in ["chat", "responses"] {
        let error = resolve("anthropic", json!({"apiMode": api_mode})).unwrap_err();
        assert_eq!(
            error,
            format!(
                "profile \"selection\": provider \"anthropic\" does not support API \"{api_mode}\""
            )
        );
    }
    assert!(resolve("openai-responses", json!({"apiMode": "chat"})).is_err());
    assert!(resolve("openaivercel", json!({"apiMode": "responses"})).is_err());
    assert!(resolve("openai-compatible", json!({"apiMode": "responses"})).is_err());
    assert!(resolve("codex", json!({"apiMode": "chat"})).is_err());
}

#[test]
fn model_names_do_not_select_an_api_and_transport_is_not_profile_selectable() {
    for model in ["gpt-4", "gpt-5.6", "anything-responses"] {
        let value = json!({"provider": "openai", "model": model});
        let provider = ProviderId::parse(value.get("provider").unwrap(), "selection").unwrap();
        let target =
            resolve_model_target(provider, value.get("ephemeralSettings"), "selection").unwrap();
        assert_eq!(target.api, ModelApi::ChatCompletions, "{model}");
        assert_eq!(target.transport, TransportKind::Http);
    }

    let target = resolve("openai", json!({"transport": "websocket"})).unwrap();
    assert_eq!(target.transport, TransportKind::Http);
}
