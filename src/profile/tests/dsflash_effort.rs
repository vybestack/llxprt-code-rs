//! dsflash effort handling: the six-value enum, the merge of the
//! `ephemeralSettings` and `chat_template_kwargs` spellings, and agreement.

use super::*;
use serde_json::json;

#[test]
fn dsflash_effort_is_enum_validated_and_merged_into_the_discriminator() {
    // kwargs effort alone.
    let kwargs_effort = parse_profile_value(
        &json!({
            "provider": "openai",
            "model": "m",
            "modelParams": {"chat_template_kwargs": {
                "enable_thinking": true,
                "reasoning_effort": "xhigh"
            }}
        }),
        "any-name",
    )
    .unwrap();
    assert_eq!(
        kwargs_effort
            .model_params
            .chat_template_kwargs
            .and_then(|spec| spec.reasoning_effort),
        Some(crate::profile::DsflashEffort::Xhigh)
    );

    // Ephemeral effort alone: validated against the six-value enum and written
    // into the discriminator spec (one-or-the-other becomes the wire effort).
    let ephemeral_effort = parse_profile_value(
        &json!({
            "provider": "openai",
            "model": "m",
            "ephemeralSettings": {"reasoning.effort": "max"},
            "modelParams": {"chat_template_kwargs": {"enable_thinking": true}}
        }),
        "any-name",
    )
    .unwrap();
    assert_eq!(
        ephemeral_effort
            .model_params
            .chat_template_kwargs
            .and_then(|spec| spec.reasoning_effort),
        Some(crate::profile::DsflashEffort::Max)
    );
    // The dsflash variant suppresses the legacy effort prompt note.
    assert!(!ephemeral_effort
        .ephemeral
        .prompt_notes
        .contains_key("reasoning:reasoning.effort"));
}

#[test]
fn dsflash_effort_agreement_and_variant_scoping() {
    // Agreement is enforced when both are present.
    parse_profile_value(
        &json!({
            "provider": "openai",
            "model": "m",
            "ephemeralSettings": {"reasoning.effort": "high"},
            "modelParams": {"chat_template_kwargs": {
                "enable_thinking": true,
                "reasoning_effort": "low"
            }}
        }),
        "any-name",
    )
    .expect_err("disagreeing efforts must reject");

    // A non-enum ephemeral effort rejects only for the dsflash variant; the
    // Standard variant keeps the legacy note (any bounded string).
    parse_profile_value(
        &json!({
            "provider": "openai",
            "model": "m",
            "ephemeralSettings": {"reasoning.effort": "seventy"},
            "modelParams": {"chat_template_kwargs": {"enable_thinking": true}}
        }),
        "any-name",
    )
    .expect_err("non-enum effort must reject for the dsflash variant");
    let standard = parse_profile_value(
        &json!({
            "provider": "openai",
            "model": "m",
            "ephemeralSettings": {"reasoning.effort": "seventy"}
        }),
        "any-name",
    )
    .unwrap();
    assert_eq!(
        standard
            .ephemeral
            .prompt_notes
            .get("reasoning:reasoning.effort")
            .map(String::as_str),
        Some("seventy")
    );

    // Malformed discriminator objects reject at parse.
    parse_profile_value(
        &json!({
            "provider": "openai",
            "model": "m",
            "modelParams": {"chat_template_kwargs": {"enable_thinking": "true"}}
        }),
        "any-name",
    )
    .expect_err("non-boolean enable_thinking must reject");
    parse_profile_value(
        &json!({
            "provider": "openai",
            "model": "m",
            "modelParams": {"chat_template_kwargs": {
                "enable_thinking": true,
                "reasoning_effort": "ultra"
            }}
        }),
        "any-name",
    )
    .expect_err("non-enum kwargs reasoning_effort must reject");
}
