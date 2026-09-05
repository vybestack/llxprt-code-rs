//! OpenAI Responses profile settings: strict typing, inert inapplicable
//! settings, and the kebab-case `modelParams` max-output folds.

use super::*;
use serde_json::json;

#[test]
fn openai_responses_settings_are_strict_and_typed() {
    let profile = parse_profile_value(
        &json!({
            "provider": "openai",
            "model": "gpt-5.6",
            "ephemeralSettings": {
                "apiMode": "responses",
                "base-url": "http://127.0.0.1:8080/v1/responses",
                "maxOutput": 4096,
                "max_output_tokens": 4096,
                "reasoning.enabled": true,
                "reasoning.effort": "high",
                "reasoning.summary": "auto",
                "text.verbosity": "medium",
                "prompt-caching": "24h"
            },
            "modelParams": {
                "apiKey": "test-key",
                "temperature": 0.25,
                "top_p": 0.75,
                "topP": 0.75,
                "maxTokens": 4096
            }
        }),
        "responses",
    )
    .unwrap();

    assert_eq!(
        profile.target.api,
        crate::model_api::target::ModelApi::Responses
    );
    assert_eq!(profile.ephemeral.max_output_tokens, Some(4096));
    assert_eq!(profile.model_params.temperature, Some(0.25));
    assert_eq!(profile.model_params.top_p, Some(0.75));
    let settings = profile.openai_responses_settings.unwrap();
    assert_eq!(
        settings.reasoning_effort,
        Some(serdes_ai::models::openai::ReasoningEffort::High)
    );
    assert_eq!(
        settings.reasoning_summary,
        Some(serdes_ai::models::openai::ReasoningSummary::Auto)
    );
    assert_eq!(
        settings.text_verbosity,
        Some(serdes_ai::models::openai::TextVerbosity::Medium)
    );
}

#[test]
fn openai_responses_rejects_inapplicable_and_conflicting_settings() {
    let invalid_settings = [
        json!({"reasoning.enabled": true, "reasoning.effort": "high"}),
        json!({"reasoning.enabled": false, "reasoning.summary": "auto"}),
        json!({"reasoning.enabled": true, "reasoning.effort": "minimal", "reasoning.summary": "auto"}),
        json!({"text.verbosity": "max"}),
        json!({"prompt-caching": "5m"}),
        json!({"seed": 1}),
        json!({"timeout": 1000}),
        json!({"previous_response_id": "stateful"}),
        json!({"maxOutput": 1, "maxTokens": 2}),
    ];
    for ephemeral in invalid_settings {
        let value = json!({
            "provider": "openai-responses",
            "model": "gpt-5.6",
            "ephemeralSettings": ephemeral
        });
        assert!(
            parse_profile_value(&value, "responses-invalid").is_err(),
            "{value}"
        );
    }
}

#[test]
fn openai_responses_folds_kebab_max_output_tokens_model_params() {
    let profile = parse_profile_value(
        &json!({
            "provider": "openai",
            "model": "gpt-5.6",
            "ephemeralSettings": {"apiMode": "responses"},
            "modelParams": {"max-output-tokens": 8192}
        }),
        "responses",
    )
    .unwrap();
    assert_eq!(profile.ephemeral.max_output_tokens, Some(8192));
}

#[test]
fn openai_responses_folds_kebab_max_tokens_model_params() {
    let profile = parse_profile_value(
        &json!({
            "provider": "openai",
            "model": "gpt-5.6",
            "ephemeralSettings": {"apiMode": "responses"},
            "modelParams": {"max-tokens": 8192}
        }),
        "responses",
    )
    .unwrap();
    assert_eq!(profile.ephemeral.max_output_tokens, Some(8192));
}

#[test]
fn openai_responses_rejects_disagreeing_max_output_model_params() {
    parse_profile_value(
        &json!({
            "provider": "openai",
            "model": "gpt-5.6",
            "ephemeralSettings": {"apiMode": "responses", "maxOutput": 4096},
            "modelParams": {"max-output-tokens": 5000}
        }),
        "responses",
    )
    .expect_err("disagreeing max-output model params must reject");
    parse_profile_value(
        &json!({
            "provider": "openai",
            "model": "gpt-5.6",
            "ephemeralSettings": {"apiMode": "responses", "max_output_tokens": 4096},
            "modelParams": {"max-tokens": 5000}
        }),
        "responses",
    )
    .expect_err("disagreeing max-output aliases must reject");
}
