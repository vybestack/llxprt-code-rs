use super::*;

mod codex_tools;
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
        crate::model_api::target::TransportKind::Http
    );
    assert_eq!(codex.ephemeral.context_limit, Some(262_144));
    assert_eq!(codex.ephemeral.max_output_tokens, Some(40_000));
    assert_eq!(codex.ephemeral.max_turns_per_prompt, Some(-1));
    assert_eq!(codex.ephemeral.loop_detection_enabled, Some(false));
    assert!(codex.codex_settings.is_some());
}

#[test]
fn zai_anthropic_fixture_resolves_messages_target() {
    let value: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/profiles/zai.anthropic.synthetic.json"
    ))
    .unwrap();
    let profile = parse_profile_value(&value, "zai").expect("z.ai profile must parse offline");

    assert_eq!(profile.provider, "anthropic");
    assert_eq!(profile.model, "glm-5.3");
    assert_eq!(
        profile.target.api,
        crate::model_api::target::ModelApi::AnthropicMessages
    );
    assert_eq!(
        profile.ephemeral.base_url.as_ref().map(RedactedUrl::full),
        Some("https://api.z.ai/api/anthropic")
    );
}

#[test]
fn unsupported_provider_coverage_uses_bedrock() {
    let value: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/profiles/unsupported.bedrock.synthetic.json"
    ))
    .unwrap();
    assert_eq!(
        parse_profile_value(&value, "unsupported-bedrock").unwrap_err(),
        "profile \"unsupported-bedrock\": unsupported provider \"bedrock\""
    );
}

#[test]
fn codex_max_turns_accepts_unlimited_and_any_positive_cap() {
    let base: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/profiles/gpt56solhigh.json"
    ))
    .unwrap();
    for (raw, expected) in [(-1_i64, -1_i64), (64, 64), (1000, 1000)] {
        let mut value = base.clone();
        value["ephemeralSettings"]["maxTurnsPerPrompt"] = json!(raw);
        let parsed = parse_profile_value(&value, "gpt56solhigh").unwrap();
        assert_eq!(parsed.ephemeral.max_turns_per_prompt, Some(expected));
    }
    for invalid in [0, -2] {
        let mut value = base.clone();
        value["ephemeralSettings"]["maxTurnsPerPrompt"] = json!(invalid);
        let error = parse_profile_value(&value, "gpt56solhigh").unwrap_err();
        assert!(
            error.contains("maxTurnsPerPrompt"),
            "rejection must name the knob: {error}"
        );
    }
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

/// `auth-key-name` names a provider key (issue 6): it is parsed and held for
/// resolution, never treated as a keyfile path. Parsing keeps the name off every
/// rendered surface, the aliases must agree, and an over-long or empty name is a
/// parse error with the fixed value-free refusal.
#[test]
fn auth_key_name_is_a_named_provider_key_reference() {
    let marker = "secure-store-provider-key";
    let p = parse_profile_value(
        &json!({
            "provider": "openai",
            "model": "m",
            "ephemeralSettings": {
                "base-url": "https://api.example.com/v1",
                "auth-key-name": marker
            }
        }),
        "named-ref",
    )
    .expect("parse keeps the named reference for resolution");
    assert_eq!(p.ephemeral.auth_key_name.as_deref(), Some(marker));
    assert_eq!(p.auth_key_name(), Some(marker));
    let rendered = format!("{p:?}");
    assert!(!rendered.contains(marker), "the value must never travel");

    // An over-long name never reaches resolution: the fixed value-free refusal
    // is a parse error and the bytes never travel.
    let over = "k".repeat(crate::redact::MAX_KEY_NAME_BYTES + 1);
    let err = parse_profile_value(
        &json!({
            "provider": "openai",
            "model": "m",
            "ephemeralSettings": {
                "base-url": "https://api.example.com/v1",
                "auth-key-name": over
            }
        }),
        "named-ref",
    )
    .expect_err("an over-long name is a profile error");
    assert_eq!(err, crate::redact::KEY_NAME_CAP_MESSAGE.to_string());
    assert!(!err.contains("kkk"), "the name never travels: {err}");

    // The aliases are one field: equal values agree and conflicting values reject.
    let aliased = parse_profile_value(
        &json!({
            "provider": "openai",
            "model": "m",
            "ephemeralSettings": {
                "base-url": "https://api.example.com/v1",
                "auth-key-name": marker,
                "authKeyName": marker
            }
        }),
        "named-ref",
    )
    .expect("equal alias values agree");
    assert_eq!(aliased.ephemeral.auth_key_name.as_deref(), Some(marker));
    assert!(parse_profile_value(
        &json!({
            "provider": "openai",
            "model": "m",
            "ephemeralSettings": {
                "base-url": "https://api.example.com/v1",
                "auth-key-name": marker,
                "api-key-name": "other"
            }
        }),
        "named-ref",
    )
    .is_err());
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
fn dsflash_variant_is_structural_and_names_never_select() {
    // Markers without the discriminator parse under ANY name (typed fields) and
    // defer the fixed class-4 diagnostic naming the lexicographically first
    // normalized marker path.
    for name in ["dsflash-mi300x", "ordinary-profile", "qwen38"] {
        let profile = parse_profile_value(
            &json!({
                "provider": "openai",
                "model": "m",
                "ephemeralSettings": {
                    "shell-replacement": true,
                    "stream-idle-timeout-ms": 0,
                    "reasoning.enabled": true,
                    "reasoning.includeInResponse": true,
                    "reasoning.includeInContext": true,
                    "reasoning.stripFromContext": "none"
                }
            }),
            name,
        )
        .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(
            profile.chat_missing_discriminator.as_deref(),
            Some("ephemeralSettings.reasoning.enabled"),
            "{name}: lexicographically first marker"
        );
        assert!(profile.model_params.chat_template_kwargs.is_none());
    }

    // The discriminator selects the dsflash variant under any name: parse
    // succeeds, the marker diagnostic is gone, and the typed fields survive.
    for name in ["dsflash-mi300x", "renamed-profile"] {
        let profile = parse_profile_value(
            &json!({
                "provider": "openai",
                "model": "m",
                "ephemeralSettings": {"shell-replacement": true},
                "modelParams": {"chat_template_kwargs": {"enable_thinking": true}}
            }),
            name,
        )
        .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(profile.chat_missing_discriminator.is_none(), "{name}");
        let kwargs = profile
            .model_params
            .chat_template_kwargs
            .as_ref()
            .unwrap_or_else(|| panic!("{name}: discriminator must survive"));
        assert!(kwargs.enable_thinking);
        assert_eq!(kwargs.reasoning_effort, None);
    }

    // The discriminator alone (no markers) still selects the variant.
    let kwargs_only = parse_profile_value(
        &json!({
            "provider": "openai",
            "model": "m",
            "modelParams": {"chat_template_kwargs": {"enable_thinking": false}}
        }),
        "plain-name",
    )
    .unwrap();
    assert!(kwargs_only.chat_missing_discriminator.is_none());
    assert!(kwargs_only
        .model_params
        .chat_template_kwargs
        .as_ref()
        .is_some_and(|spec| !spec.enable_thinking));
}

#[test]
fn dsflash_marker_types_are_structural_not_name_gated() {
    // Correct types parse under any name; wrong types reject under any name.
    let typed = [
        ("shell-replacement", json!(true), json!("bash")),
        ("stream-idle-timeout-ms", json!(0), json!("1000")),
        ("streamIdleTimeoutMs", json!(250), json!(true)),
        ("reasoning.enabled", json!(false), json!("on")),
        ("reasoning.includeInResponse", json!(true), json!("yes")),
        ("reasoning.includeInContext", json!(true), json!(1)),
        (
            "reasoning.stripFromContext",
            json!("none"),
            json!("context"),
        ),
    ];
    for (key, good, bad) in typed {
        for name in ["dsflash-mi300x", "ordinary-profile"] {
            parse_profile_value(
                &json!({
                    "provider": "openai",
                    "model": "m",
                    "ephemeralSettings": {key: good.clone()}
                }),
                name,
            )
            .unwrap_or_else(|error| panic!("{key} {name} good: {error}"));
            parse_profile_value(
                &json!({
                    "provider": "openai",
                    "model": "m",
                    "ephemeralSettings": {key: bad.clone()}
                }),
                name,
            )
            .expect_err(&format!("{key} {name} bad type must reject"));
        }
    }

    // Kebab/camel stream-idle aliases must agree when duplicated.
    parse_profile_value(
        &json!({
            "provider": "openai",
            "model": "m",
            "ephemeralSettings": {
                "stream-idle-timeout-ms": 500,
                "streamIdleTimeoutMs": 500
            }
        }),
        "any-name",
    )
    .unwrap();
    parse_profile_value(
        &json!({
            "provider": "openai",
            "model": "m",
            "ephemeralSettings": {
                "stream-idle-timeout-ms": 500,
                "streamIdleTimeoutMs": 501
            }
        }),
        "any-name",
    )
    .expect_err("disagreeing stream-idle aliases must reject");
}

#[test]
fn old_name_gated_dsflash_keys_are_unsupported_under_every_name() {
    for (key, value) in [
        ("maxRetrywait", json!(1000)),
        ("reasoning.maxTokens", json!(1000)),
        ("reasoning.budgetTokens", json!(1000)),
        ("autokimi-style", json!(1)),
        ("sandbox-base-url", json!("https://sandbox.invalid")),
        ("default-tools", json!("all")),
        ("reasoning.effortWireFormat", json!("string")),
        ("reasoning.enabledWireFormat", json!("boolean")),
        ("reasoning.enabledMap", json!("enabled")),
        ("reasoning.effortMap", json!("effort")),
        ("reasoning.format", json!("text")),
        ("reasoning.fieldName", json!("reasoning")),
        ("reasoning.update", json!(true)),
        ("reasoning.display", json!(true)),
    ] {
        for name in ["dsflash-mi300x", "ordinary-profile"] {
            let profile = parse_profile_value(
                &json!({
                    "provider": "openai",
                    "model": "m",
                    "ephemeralSettings": {key: value.clone()}
                }),
                name,
            )
            .unwrap_or_else(|error| panic!("{key} {name}: {error}"));
            assert!(
                profile.ephemeral.unsupported.contains(&key.to_string()),
                "{key} {name}: must be unsupported, not applied"
            );
        }
    }
}

#[test]
fn common_metadata_is_exact_and_name_independent() {
    // emojifilter: exact "auto" metadata under any name; any other value rejects.
    for name in ["dsflash-mi300x", "ordinary-profile"] {
        parse_profile_value(
            &json!({
                "provider": "openai",
                "model": "m",
                "ephemeralSettings": {"emojifilter": "auto"}
            }),
            name,
        )
        .unwrap_or_else(|error| panic!("{name}: {error}"));
        parse_profile_value(
            &json!({
                "provider": "openai",
                "model": "m",
                "ephemeralSettings": {"emojifilter": "on"}
            }),
            name,
        )
        .expect_err(&format!("{name}: non-auto emojifilter must reject"));
    }

    // requires-auth: boolean metadata under any name.
    for value in [json!(true), json!(false)] {
        parse_profile_value(
            &json!({
                "provider": "openai",
                "model": "m",
                "ephemeralSettings": {"requires-auth": value}
            }),
            "ordinary-profile",
        )
        .unwrap();
    }
    parse_profile_value(
        &json!({
            "provider": "openai",
            "model": "m",
            "ephemeralSettings": {"requires-auth": "yes"}
        }),
        "ordinary-profile",
    )
    .expect_err("string requires-auth must reject");

    // tool-format aliases: equal values parse (value judgment is deferred),
    // disagreeing duplicates reject.
    parse_profile_value(
        &json!({
            "provider": "openai",
            "model": "m",
            "ephemeralSettings": {"tool-format": "auto", "toolFormat": "auto"}
        }),
        "any-name",
    )
    .unwrap();
    parse_profile_value(
        &json!({
            "provider": "openai",
            "model": "m",
            "ephemeralSettings": {"tool-format": "auto", "toolFormat": "openai"}
        }),
        "any-name",
    )
    .expect_err("disagreeing tool-format aliases must reject");
}

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
