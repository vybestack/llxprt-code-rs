use super::*;
use serde_json::json;

#[test]
fn configuration_selectors_require_nonempty_absolute_paths() {
    assert!(require_absolute_path("TEST_CONFIG", PathBuf::new()).is_err());
    assert!(require_absolute_path("TEST_CONFIG", PathBuf::from("relative")).is_err());
    assert_eq!(
        require_absolute_path("TEST_CONFIG", PathBuf::from("/absolute/config")).unwrap(),
        PathBuf::from("/absolute/config")
    );
}

#[test]
fn top_level_profile_shape_is_strict() {
    for accepted in [
        json!({"provider": "openai", "model": "m"}),
        json!({"version": 1, "type": "standard", "provider": "openai", "model": "m"}),
        json!({"provider": "openai", "model": "m", "name": "display", "_note": "fixture"}),
    ] {
        parse_profile_value(&accepted, "shape").expect("valid profile shape");
    }

    for rejected in [
        json!({"version": 2, "provider": "openai", "model": "m"}),
        json!({"version": "1", "provider": "openai", "model": "m"}),
        json!({"type": "other", "provider": "openai", "model": "m"}),
        json!({"provider": "openai", "model": "m", "unknown": true}),
        json!({"provider": "openai", "model": "m", "name": false}),
    ] {
        assert!(parse_profile_value(&rejected, "shape").is_err());
    }

    for load_balancer in [
        json!({"type": "loadbalancer", "provider": "openai", "model": "m"}),
        json!({"provider": "openai", "model": "m", "profiles": []}),
    ] {
        assert_eq!(
            parse_profile_value(&load_balancer, "shape").unwrap_err(),
            parsing::LOAD_BALANCER_UNSUPPORTED_MESSAGE
        );
    }

    let auth = json!({"provider": "openai", "model": "m", "auth": {}});
    assert_eq!(
        parse_profile_value(&auth, "shape").unwrap_err(),
        parsing::TOP_LEVEL_AUTH_UNSUPPORTED_MESSAGE
    );
}
/// A strict profile type table: `ephemeralSettings`/`modelParams` must be JSON

#[test]
fn parsed_profile_stores_the_resolved_target() {
    let chat =
        parse_profile_value(&json!({"provider": "openai", "model": "gpt-5.6"}), "chat").unwrap();
    assert_eq!(
        chat.target.api,
        crate::model_api::target::ModelApi::ChatCompletions
    );
    assert_eq!(
        chat.target.transport,
        crate::model_api::target::TransportKind::Http
    );

    let codex_value: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/profiles/gpt56solhigh.json"
    ))
    .unwrap();
    let codex = parse_profile_value(&codex_value, "gpt56solhigh").unwrap();
    assert_eq!(
        codex.target.api,
        crate::model_api::target::ModelApi::Responses
    );
    assert_eq!(
        codex.target.transport,
        crate::model_api::target::TransportKind::WebSocket
    );
    assert_eq!(codex.ephemeral.context_limit, Some(262_144));
    assert_eq!(codex.ephemeral.max_output_tokens, Some(40_000));
    assert_eq!(codex.ephemeral.max_turns_per_prompt, Some(-1));
    assert_eq!(codex.ephemeral.loop_detection_enabled, Some(false));
    assert!(codex.codex_settings.is_some());
}

#[test]
fn codex_loop_detection_cannot_be_silently_ignored() {
    let mut value: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/profiles/gpt56solhigh.json"
    ))
    .unwrap();
    value["ephemeralSettings"]["loopDetectionEnabled"] = json!(true);

    let error = parse_profile_value(&value, "gpt56solhigh").unwrap_err();

    assert_eq!(
        error,
        "profile \"gpt56solhigh\": Codex loop detection is not supported by this runtime"
    );
}

/// objects when present, and each known scalar field must have the right type. Every
/// bound field stays error-on-wrong-type, never a silent ignore.
#[test]
fn ephemeral_and_modelparam_strict_type_table() {
    // `ephemeralSettings` non-object.
    let p = parse_profile_value(
        &json!({"provider":"openai","model":"m","ephemeralSettings":[]}),
        "bad",
    );
    assert!(p.is_err(), "ephemeralSettings array must be rejected");
    // Each known scalar field is type-enforced.
    for (k, v) in [
        ("base-url", json!(5)),
        ("baseUrl", json!(true)),
        ("auth-key", json!(1)),
        ("authKey", json!([])),
        ("auth-keyfile", json!(0)),
        ("context-limit", json!("many")),
        ("maxOutputTokens", json!("lots")),
        ("stream-first-response-timeout-ms", json!({"ms":1})),
    ] {
        let p = parse_profile_value(
            &json!({"provider":"openai","model":"m",
                       "ephemeralSettings": {k: v}}),
            "bad",
        );
        assert!(p.is_err(), "ephemeral '{k}' wrong type must be rejected");
    }
    // `modelParams` non-object.
    let p = parse_profile_value(
        &json!({"provider":"openai","model":"m","modelParams":42}),
        "bad",
    );
    assert!(p.is_err(), "modelParams non-object must be rejected");
    for (k, v) in [
        ("temperature", json!("hot")),
        ("top_p", json!("p")),
        ("topP", json!([])),
        ("seed", json!("s")),
    ] {
        let p = parse_profile_value(
            &json!({"provider":"openai","model":"m",
                       "modelParams": {k: v}}),
            "bad",
        );
        assert!(p.is_err(), "modelparam '{k}' wrong type must be rejected");
    }
    // The well-typed form still parses.
    let p = parse_profile_value(
        &json!({"provider":"openai","model":"m",
        "modelParams": {"temperature":0.2,"top_p":0.9,"seed":7},
        "ephemeralSettings": {
            "base-url":"http://127.0.0.1:1/v1",
            "auth-key":"k",
            "context-limit":100,
            "maxOutputTokens":16384,
            "stream-first-response-timeout-ms":30000
        }}),
        "ok",
    );
    let p = p.expect("valid profile must parse");
    assert_eq!(p.model_params.temperature, Some(0.2));
    assert_eq!(p.model_params.top_p, Some(0.9));
    assert_eq!(p.model_params.seed, Some(7));
    assert_eq!(p.ephemeral.context_limit, Some(100));
    assert_eq!(p.ephemeral.max_output_tokens, Some(16384));
    assert_eq!(p.ephemeral.timeout_ms, Some(30000));
}
#[test]
fn model_identifier_rejects_empty_whitespace_and_controls() {
    for model in ["", "   \t", "model\nname", "model\u{7f}name"] {
        let result =
            parse_profile_value(&json!({"provider": "openai", "model": model}), "bad-model");
        assert!(
            result.is_err(),
            "invalid model identifier {model:?} must fail"
        );
    }
    for model in ["gpt-5.6", "owner/model_name:v1"] {
        let profile = parse_profile_value(
            &json!({"provider": "openai", "model": model}),
            "valid-model",
        )
        .expect("valid model punctuation must parse");
        assert_eq!(profile.model, model);
    }
}

#[test]
fn keyfile_aliases_are_credentials_and_debug_is_redacted() {
    for alias in ["auth-keyfile", "authKeyfile", "apiKeyfile"] {
        let marker = format!("/private/credential/{alias}-marker.key");
        let profile = parse_profile_value(
            &json!({
                "provider": "openai",
                "model": "m",
                "ephemeralSettings": {alias: marker}
            }),
            "aliases",
        )
        .expect("keyfile alias must parse as a credential path");
        assert_eq!(
            profile.ephemeral.auth_keyfile_orig.as_deref(),
            Some(marker.as_str()),
            "{alias}"
        );
        assert!(
            !profile.ephemeral.prompt_notes.contains_key(alias),
            "{alias}"
        );
        let rendered = format!("{:?}", profile.ephemeral);
        assert!(!rendered.contains(&marker), "{alias}: {rendered}");
        assert!(
            !rendered.contains(&format!("{alias}-marker.key")),
            "{alias}: {rendered}"
        );
    }
}

/// `auth-key-name` is a named **secure-store** reference, never a keyfile path: it
/// fails parsing with the fixed value-free refusal (its bytes never travel), and a
/// same-named local file is never even considered as a keyfile.
#[test]
fn auth_key_name_is_an_unsupported_secure_store_reference() {
    let marker = "secure-store-provider-key";
    let p = parse_profile_value(
        &json!({
            "provider": "openai",
            "model": "m",
            "ephemeralSettings": {"auth-key-name": marker}
        }),
        "named-ref",
    );
    let err = p.expect_err("auth-key-name must fail parsing");
    assert_eq!(err, AUTH_KEY_NAME_UNSUPPORTED_MESSAGE);
    assert!(!err.contains(marker), "the value must never travel: {err}");
}

/// The strict endpoint shape: a non-http(s) scheme, userinfo, query, fragment,
/// or a non-URL each fail so the configured endpoint is never ambiguous. (A
/// non-URL never renders the raw value; everything still collapses to the redacted
/// form on error surfaces.)
#[test]
fn base_url_strict_rejection_table() {
    for raw in [
        "ftp://127.0.0.1/x",
        "https://alice:secret@api.example.com/v1",
        "http://127.0.0.1/v1?q=1",
        "http://127.0.0.1/v1#frag",
        "not a url",
    ] {
        let p = parse_profile_value(
            &json!({"provider":"openai","model":"m",
                       "ephemeralSettings":{"base-url":raw,"auth-key":"k"}}),
            "bad",
        );
        assert!(p.is_err(), "base-url {raw:?} must be rejected");
    }
    // A well-formed loopback base-url still parses (strict shape kept).
    let p = parse_profile_value(
        &json!({"provider":"openai","model":"m",
                   "ephemeralSettings":{"base-url":"http://127.0.0.1:1/v1","auth-key":"k"}}),
        "ok",
    );
    assert!(p.is_ok(), "a valid loopback base-url must parse");
    // The stored `scheme://host:port` rendering stays verbatim for the transport
    // (so routing/billing reach the real endpoint) but the full value never carries
    // userinfo/query/fragment.
    let p = parse_profile_value(
        &json!({"provider":"openai","model":"m",
                   "ephemeralSettings":{"base-url":"https://api.example.com/v1","auth-key":"k"}}),
        "ok",
    )
    .expect("a conventional path-prefix base-url must parse");
    assert_eq!(
        p.ephemeral
            .base_url
            .as_ref()
            .map(|u| u.full().to_string())
            .as_deref(),
        Some("https://api.example.com/v1")
    );
}

#[test]
fn public_redacted_url_constructor_enforces_endpoint_policy() {
    let secret = "constructor-secret-marker";
    for raw in [
        "ftp://127.0.0.1/x".to_string(),
        format!("https://alice:{secret}@api.example.com/v1"),
        format!("https://api.example.com/v1?token={secret}"),
        format!("https://api.example.com/v1#{secret}"),
    ] {
        let err = RedactedUrl::parse(&raw).expect_err("unsafe public URL must reject");
        assert!(!err.contains(secret));
        assert!(!format!("{err:?}").contains(secret));
    }
    let valid = RedactedUrl::parse("https://api.example.com/v1").unwrap();
    assert_eq!(valid.as_display(), "https://api.example.com");
}

#[test]
fn reasoning_effort_enforces_prompt_note_cap() {
    let exact = "x".repeat(crate::redact::MAX_PROMPT_NOTE_BYTES);
    let profile = parse_profile_value(
        &json!({
            "provider": "openai",
            "model": "m",
            "ephemeralSettings": {"reasoning.effort": exact}
        }),
        "bounded",
    )
    .unwrap();
    assert_eq!(
        profile.ephemeral.prompt_notes["reasoning:reasoning.effort"].len(),
        crate::redact::MAX_PROMPT_NOTE_BYTES
    );

    let over = "x".repeat(crate::redact::MAX_PROMPT_NOTE_BYTES + 1);
    let error = parse_profile_value(
        &json!({
            "provider": "openai",
            "model": "m",
            "ephemeralSettings": {"reasoning.effort": over}
        }),
        "bounded",
    )
    .unwrap_err();
    assert_eq!(error, crate::redact::PROMPT_NOTE_CAP_MESSAGE);
}

#[test]
fn ignored_behavior_settings_are_limited_to_dsflash_profiles() {
    assert!(is_dsflash_profile_name("dsflash"));
    assert!(is_dsflash_profile_name("/profiles/dsflash-mi300x.json"));
    for near_match in [
        "ordinary-dsflash",
        "dsflashlike",
        "deepseek",
        "/profiles/not-dsflash.json",
    ] {
        assert!(!is_dsflash_profile_name(near_match), "{near_match}");
    }
    let settings = [
        ("emojifilter", json!("on")),
        ("shell-replacement", json!("bash")),
        ("stream-idle-timeout-ms", json!("1000")),
        ("requires-auth", json!(true)),
        ("streamIdleTimeoutMs", json!(1000)),
        ("maxRetrywait", json!(1000)),
        ("reasoning.maxTokens", json!(1000)),
        ("reasoning.budgetTokens", json!(1000)),
        ("autokimi-style", json!(1)),
        ("sandbox-base-url", json!("https://sandbox.invalid")),
        ("default-tools", json!("all")),
        ("tool-format", json!("json")),
        ("reasoning.enabled", json!(true)),
        ("reasoning.includeInResponse", json!(true)),
        ("reasoning.includeInContext", json!(true)),
        ("reasoning.stripFromContext", json!(true)),
        ("reasoning.effortWireFormat", json!("string")),
        ("reasoning.enabledWireFormat", json!("boolean")),
        ("reasoning.enabledMap", json!("enabled")),
        ("reasoning.effortMap", json!("effort")),
        ("reasoning.format", json!("text")),
        ("reasoning.fieldName", json!("reasoning")),
        ("reasoning.update", json!(true)),
        ("reasoning.display", json!(true)),
    ];
    for (key, value) in settings {
        let ordinary = parse_profile_value(
            &json!({
                "provider": "openai",
                "model": "m",
                "ephemeralSettings": {key: value.clone()}
            }),
            "ordinary-profile",
        );
        let error = ordinary.expect_err("ignored behavior must fail outside dsflash");
        assert!(
            error.contains("only supported for dsflash profiles"),
            "{key}: {error}"
        );

        let dsflash = parse_profile_value(
            &json!({
                "provider": "openai",
                "model": "m",
                "ephemeralSettings": {key: value}
            }),
            "dsflash-mi300x",
        )
        .unwrap_or_else(|error| panic!("{key}: {error}"));
        assert!(dsflash.ephemeral.is_dsflash, "{key}");
    }
}
