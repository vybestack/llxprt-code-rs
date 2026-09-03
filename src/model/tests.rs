use crate::model::{check_http_policy, classify_loopback, parse_base_url, validate_base_url};
use crate::profile::parse_profile_value;
use serde_json::json;

fn base_profile() -> crate::profile::Profile {
    parse_profile_value(
        &json!({
            "provider": "openai",
            "model": "deepseek-ai/DeepSeek-V4-Flash-0731",
            "modelParams": { "temperature": 0.1 },
            "ephemeralSettings": {
                "base-url": "http://127.0.0.1:8080/v1",
                "auth-keyfile": "~/nope.key"
            }
        }),
        "test",
    )
    .unwrap()
}

/// Parse `json` and run it through `from_profile_in`, returning the rendered
/// error (every sub-case below asserts a failure).
fn load_err(json: &serde_json::Value, name: &str, allow_insecure: bool) -> String {
    let profile = parse_profile_value(json, name).unwrap();
    crate::model::ModelConfig::from_profile_in(
        &profile,
        false,
        allow_insecure,
        std::path::Path::new("."),
    )
    .unwrap_err()
    .to_string()
}

/// The rendered credential-policy failure for an unresolvable named provider key,
/// as `from_profile_in` reports it (issue 6).
const NAMED_KEY_RESOLUTION_FAILURE: &str =
    "auth-key-name could not be resolved; set LLXPRT_PROVIDER_KEY_<NAME> or store the key under the llxprt-code-provider-keys secure-store account";
/// The **public** strict validator accepts a bare origin, `/v1`, and the chat-route
/// forms (the full URL, including its path, is what is validated) with a host and
/// no userinfo/query/fragment, rejects a nested path, userinfo, query, and
/// fragment, and that rejection always stays sanitized/value-free.
#[test]
fn public_validate_base_url_full_url_routes_and_value_free_errors() {
    for raw in [
        "https://api.example.com",
        "https://api.example.com/",
        "https://api.example.com/v1",
        "https://api.example.com/v1/",
        "https://api.example.com/chat/completions",
        "https://api.example.com/v1/chat/completions",
    ] {
        let url = crate::profile::RedactedUrl::from_unvalidated(raw);
        let cfg = crate::model::ModelConfig {
            model: "m".into(),
            base_url: url.clone(),
            api_key: "k".into(),
            keyfile_path: None,
            max_output_tokens: None,
            timeout: None,
            model_params: None,
            context_limit: None,
        };
        assert!(
            validate_base_url(url.full()).is_ok(),
            "public validator must accept {raw}"
        );
        assert!(cfg.validate_url().is_ok(), "validate_url must accept {raw}");
    }
    for raw in [
        "https://api.example.com/inference/v1",
        "https://user@api.example.com/v1",
        "https://:pass@api.example.com",
        "https://api.example.com/v1?key=secret",
        "https://api.example.com/v1#frag",
        "https://api.example.com/inference/v1/chat/completions",
    ] {
        let url = crate::profile::RedactedUrl::from_unvalidated(raw);
        let cfg = crate::model::ModelConfig {
            model: "m".into(),
            base_url: url.clone(),
            api_key: "k".into(),
            keyfile_path: None,
            max_output_tokens: None,
            timeout: None,
            model_params: None,
            context_limit: None,
        };
        let err = validate_base_url(url.full())
            .expect_err("a nested/userinfo/query/fragment URL must be rejected");
        assert!(
            !err.to_string().contains("api.example.com"),
            "the error must stay value-free: {err}"
        );
        assert!(!err.to_string().contains("secret"), "value-free: {err}");
        assert!(
            cfg.validate_url().is_err(),
            "validate_url must reject {raw}"
        );
    }
    // The **display** form hides the path, so a URL whose full path is an
    // unsupported nested route must be rejected by the public validator (this is
    // the finding: the redacted display used to hide it).
    assert!(validate_base_url("https://api.example.com/inference/v1/chat/completions").is_err());
}

#[test]
fn strict_parse_rejects_wrong_types() {
    use crate::model::ModelConfig;
    let bad = parse_profile_value(
        &json!({"provider": "openai", "model": "m", "modelParams": []}),
        "bad",
    );
    assert!(bad.is_err());
    let bad = parse_profile_value(
        &json!({"provider": "openai", "model": "m",
               "ephemeralSettings": {"base-url": {"x": 1}}}),
        "bad",
    );
    assert!(bad.is_err());
    let bad = parse_profile_value(
        &json!({"provider": "openai", "model": "m",
               "ephemeralSettings": {"maxOutputTokens": "lots"}}),
        "bad",
    );
    assert!(bad.is_err());
    let bad = parse_profile_value(
        &json!({"provider": "openai", "model": "m",
               "ephemeralSettings": {"stream-first-response-timeout-ms": "long"}}),
        "bad",
    );
    assert!(bad.is_err());
    for p in [
        "http://127.0.0.1:1/v1",
        "http://[::1]:1/v1",
        "http://localhost:1/v1",
        "https://api.example.com/v1",
    ] {
        let bp = base_profile();
        let p = crate::profile::Profile {
            ephemeral: crate::profile::EphemeralSettings {
                base_url: Some(crate::profile::RedactedUrl::from_unvalidated(p)),
                auth_key: Some("k".into()),
                ..Default::default()
            },
            ..bp
        };
        assert!(ModelConfig::from_profile(&p, true, false).is_ok());
    }
    assert!(ModelConfig::from_profile(
        &crate::profile::Profile {
            ephemeral: crate::profile::EphemeralSettings {
                base_url: Some(crate::profile::RedactedUrl::from_unvalidated(
                    "http://23.183.40.76:8080/v1"
                )),
                auth_key: Some("k".into()),
                ..Default::default()
            },
            ..base_profile()
        },
        true,
        false,
    )
    .is_err());
    assert!(ModelConfig::from_profile(
        &crate::profile::Profile {
            ephemeral: crate::profile::EphemeralSettings {
                base_url: Some(crate::profile::RedactedUrl::from_unvalidated(
                    "http://23.183.40.76:8080/v1"
                )),
                auth_key: Some("k".into()),
                ..Default::default()
            },
            ..base_profile()
        },
        true,
        true,
    )
    .is_ok());
}

#[test]
fn whitespace_only_inline_auth_is_rejected_without_normalizing_other_keys() {
    let mut profile = base_profile();
    profile.ephemeral.auth_key = Some(" \t\n ".into());
    assert!(matches!(
        super::resolve_api_key(&profile, true, std::path::Path::new("/unused")),
        Err(super::ModelError::NoAuth)
    ));

    profile.ephemeral.auth_key = Some("  key bytes  ".into());
    assert_eq!(
        super::resolve_api_key(&profile, true, std::path::Path::new("/unused")).unwrap(),
        "  key bytes  "
    );
}

#[test]
fn keyfile_content_enforces_exact_4096_byte_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let keyfile = dir.path().join("boundary.key");
    std::fs::write(&keyfile, "k".repeat(crate::redact::MAX_KEY_BYTES)).unwrap();
    assert_eq!(
        super::read_keyfile_bounded(keyfile.to_str().unwrap())
            .unwrap()
            .len(),
        crate::redact::MAX_KEY_BYTES
    );

    std::fs::write(&keyfile, "k".repeat(crate::redact::MAX_KEY_BYTES + 1)).unwrap();
    let error = super::read_keyfile_bounded(keyfile.to_str().unwrap()).unwrap_err();
    let rendered = error.to_string();
    assert_eq!(rendered, crate::redact::KEY_CAP_MESSAGE);
    assert!(!rendered.contains(keyfile.to_str().unwrap()));
}

#[test]
fn parse_base_url_scheme_host_rules() {
    assert!(parse_base_url("http://127.0.0.1:1/v1").is_ok());
    assert!(parse_base_url("https://api.example.com/v1").is_ok());
    assert!(parse_base_url("ftp://127.0.0.1/x").is_err());
    assert!(parse_base_url("127.0.0.1:8080").is_err());
    assert!(classify_loopback("http://[::1]:8080/v1"));
    assert!(classify_loopback("http://localhost:8080/v1"));
    assert!(!classify_loopback("http://23.183.40.76:8080/v1"));
}

#[test]
fn insecure_http_error_does_not_retain_the_endpoint() {
    let endpoint = "http://user:secret@example.com/private?api-key=value#token";
    let error = check_http_policy(endpoint, false).expect_err("plaintext remote URL must fail");
    let display = error.to_string();
    let debug = format!("{error:?}");
    for secret in [
        "user",
        "secret",
        "example.com",
        "private",
        "api-key",
        "value",
        "token",
    ] {
        assert!(!display.contains(secret), "display retained {secret:?}");
        assert!(!debug.contains(secret), "debug retained {secret:?}");
    }
}

#[test]
fn unknown_output_setting_fails() {
    use crate::model::ModelError;
    use crate::profile::ModelParams;
    let inner = crate::profile::Profile {
        ephemeral: crate::profile::EphemeralSettings {
            base_url: Some(crate::profile::RedactedUrl::from_unvalidated(
                "http://127.0.0.1:1/v1",
            )),
            auth_key: Some("k".into()),
            ..Default::default()
        },
        model_params: ModelParams {
            temperature: None,
            top_p: None,
            top_k: None,
            seed: None,
            chat_template_kwargs: None,
            unsupported: vec!["stop".into()],
        },
        ..base_profile()
    };
    let err = crate::model::ModelConfig::from_profile(&inner, true, true).unwrap_err();
    assert!(matches!(err, ModelError::UnsupportedSetting(_)));
}

/// PLAN.md line 38: every configuration error class outranks credential
/// resolution, in a fixed order (endpoint > credential policy > structural
/// dsflash gate > target settings > key resolution).
#[test]
fn configuration_errors_precede_credential_resolution_in_fixed_order() {
    // Nested path (class 2) outranks the keyfile read.
    let nested = serde_json::json!({
        "provider": "openai",
        "model": "m",
        "ephemeralSettings": {
            "base-url": "https://api.example.com/nested/path",
            "auth-keyfile": "~/definitely-missing.key"
        }
    });
    assert_eq!(
        load_err(&nested, "ordering", false),
        "unsupported or invalid endpoint: unsupported or invalid endpoint"
    );

    // Plaintext HTTP to a non-loopback host refuses before the keyfile read.
    let insecure = serde_json::json!({
        "provider": "openai",
        "model": "m",
        "ephemeralSettings": {
            "base-url": "http://api.example.com/v1",
            "auth-keyfile": "~/definitely-missing.key"
        }
    });
    assert_eq!(
        load_err(&insecure, "ordering", false),
        "insecure http base-url requires --allow-insecure-http"
    );

    // Credential policy (class 3) precedes the structural gate (class 4): an
    // unresolvable named key is reported before a marker without a discriminator.
    let both = serde_json::json!({
        "provider": "openai",
        "model": "m",
        "ephemeralSettings": {
            "base-url": "https://api.example.com/v1",
            "auth-key-name": "ordering-unresolved-marker",
            "shell-replacement": true
        }
    });
    assert_eq!(
        load_err(&both, "ordering", false),
        format!(
            "unsupported profile setting(s): {}",
            NAMED_KEY_RESOLUTION_FAILURE
        )
    );

    // The structural gate (class 4) precedes unsupported-key rejection
    // (class 6), which in turn precedes the keyfile read.
    let marker_and_unsupported = serde_json::json!({
        "provider": "openai",
        "model": "m",
        "ephemeralSettings": {
            "base-url": "https://api.example.com/v1",
            "shell-replacement": true,
            "maxRetrywait": 1000,
            "auth-keyfile": "~/definitely-missing.key"
        }
    });
    assert_eq!(
        load_err(&marker_and_unsupported, "ordering", false),
        "unsupported profile setting(s): \
             ephemeralSettings.shell-replacement is a dsflash-only chat setting and \
             requires modelParams.chat_template_kwargs"
    );
}

/// The API-kind gate and the profile-name check are pure: neither depends on
/// a resolvable configuration root, and the gate names the provider.
#[test]
fn api_gate_and_name_validity_do_not_touch_the_config_root() {
    let responses = parse_profile_value(
        &serde_json::json!({
            "provider": "openai",
            "model": "m",
            "ephemeralSettings": {
                "apiMode": "responses",
                "base-url": "https://api.example.com/v1"
            }
        }),
        "gate",
    )
    .unwrap();
    let err = crate::model::ModelConfig::from_profile_in(
        &responses,
        false,
        false,
        std::path::Path::new("."),
    )
    .unwrap_err()
    .to_string();
    assert_eq!(
        err,
        "provider openai selects a non-chat-completions API; \
         this path supports chat completions only"
    );

    // An invalid name is Missing for the resolver regardless of the root,
    // before any directory is resolved or read.
    let resolver = crate::model::ProfileResolver;
    for name in ["", "../escape", "has space", "dot.name"] {
        assert!(
            matches!(
                resolver
                    .load_in(name, std::path::Path::new("/nonexistent-root"))
                    .unwrap(),
                crate::model::ResolveOutcome::Missing(_)
            ),
            "{name:?} must be Missing without touching the root"
        );
    }
}

/// Vercel Chat accepts only the Standard variant; the dsflash variant
/// rejects in class 6, before any credential resolution.
#[test]
fn vercel_chat_rejects_the_dsflash_variant_before_credentials() {
    let profile = parse_profile_value(
        &serde_json::json!({
            "provider": "openaivercel",
            "model": "m",
            "ephemeralSettings": {
                "base-url": "https://api.example.com/v1",
                "shell-replacement": true
            },
            "modelParams": {
                "chat_template_kwargs": {"enable_thinking": true}
            }
        }),
        "vercel-dsflash",
    )
    .unwrap();
    assert_eq!(
        profile.target.provider,
        crate::model_api::target::ProviderId::OpenAiVercel
    );
    let err = crate::model::ModelConfig::from_profile_in(
        &profile,
        false,
        false,
        std::path::Path::new("."),
    )
    .unwrap_err()
    .to_string();
    assert_eq!(
        err,
        "unsupported profile setting(s): \
         dsflash chat settings are not supported on OpenAI Vercel Chat targets"
    );
}

/// The friendliglm ladder: the installed shape rejects on its arbitrary
/// route prefix first; each rung peels one cause until the settings-accepted
/// shape parses and builds a dsflash ModelConfig.
#[test]
fn friendliglm_ladder_first_failures_in_order() {
    let installed: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/profiles/friendliglm.json"
    ))
    .unwrap();

    // Rung 1 (installed): the /serverless/v1 route is an arbitrary path
    // prefix; that refuses before the named secure-store reference.
    let profile = parse_profile_value(&installed, "friendliglm").unwrap();
    let err = crate::model::ModelConfig::from_profile_in(
        &profile,
        false,
        false,
        std::path::Path::new("."),
    )
    .unwrap_err();
    assert!(
        matches!(err, crate::model::ModelError::InvalidEndpoint(_)),
        "{err}"
    );

    // Rung 2: rewrite the route to /v1 (host preserved); the unresolvable named
    // key now refuses first.
    let mut rung2 = installed.clone();
    rung2["ephemeralSettings"]["base-url"] = serde_json::json!("https://api.friendli.ai/v1");
    let profile = parse_profile_value(&rung2, "friendliglm").unwrap();
    let err = crate::model::ModelConfig::from_profile_in(
        &profile,
        false,
        false,
        std::path::Path::new("."),
    )
    .unwrap_err()
    .to_string();
    assert_eq!(
        err,
        format!(
            "unsupported profile setting(s): {}",
            NAMED_KEY_RESOLUTION_FAILURE
        )
    );

    // Rung 3: drop auth-key-name; the unsupported model parameters are now
    // the first failure, in source order.
    let mut rung3 = rung2.clone();
    rung3["ephemeralSettings"]
        .as_object_mut()
        .unwrap()
        .remove("auth-key-name");
    let profile = parse_profile_value(&rung3, "friendliglm").unwrap();
    let err = crate::model::ModelConfig::from_profile_in(
        &profile,
        false,
        false,
        std::path::Path::new("."),
    )
    .unwrap_err()
    .to_string();
    assert_eq!(
        err,
        "unsupported profile setting(s): max_tokens, parse_reasoning"
    );

    // Rung 4: drop both unsupported model parameters; the dsflash
    // discriminator survives and the config resolves with an inline key.
    let mut rung4 = rung3.clone();
    rung4["modelParams"]
        .as_object_mut()
        .unwrap()
        .remove("parse_reasoning");
    rung4["modelParams"]
        .as_object_mut()
        .unwrap()
        .remove("max_tokens");
    rung4["ephemeralSettings"]["auth-key"] = serde_json::json!("inline-test-key");
    let profile = parse_profile_value(&rung4, "friendliglm").unwrap();
    let config = crate::model::ModelConfig::from_profile_in(
        &profile,
        false,
        false,
        std::path::Path::new("."),
    )
    .expect("the accepted rung builds a dsflash config");
    let kwargs = config
        .model_params
        .as_ref()
        .and_then(|params| params.chat_template_kwargs.as_ref())
        .expect("the discriminator survives resolution");
    assert!(kwargs.enable_thinking);
}

/// First-failure rows for the remaining marker-bearing installed profiles:
/// qwen38 (named reference first), qwen38-mi300x and ornith-runpod (marker
/// diagnostics), and chutesk2streaming (marker diagnostic before its
/// unsupported `streaming` key).
#[test]
fn marker_profiles_fail_on_their_first_structural_cause() {
    let qwen38: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/fixtures/profiles/qwen38.json")).unwrap();
    assert_eq!(
        load_err(&qwen38, "qwen38", false),
        format!(
            "unsupported profile setting(s): {}",
            NAMED_KEY_RESOLUTION_FAILURE
        )
    );

    let mi300x: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/profiles/qwen38-mi300x.json"
    ))
    .unwrap();
    // The fixture endpoint is plaintext http on a remote host; the flag is the
    // production knob for that and must not reorder the classes.
    assert_eq!(
        load_err(&mi300x, "qwen38-mi300x", true),
        "unsupported profile setting(s): \
         ephemeralSettings.shell-replacement is a dsflash-only chat setting and \
         requires modelParams.chat_template_kwargs"
    );

    let ornith: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/profiles/ornith-runpod.json"
    ))
    .unwrap();
    assert_eq!(
        load_err(&ornith, "ornith-runpod", false),
        "unsupported profile setting(s): \
         ephemeralSettings.stream-idle-timeout-ms is a dsflash-only chat setting and \
         requires modelParams.chat_template_kwargs"
    );

    let chutes: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/profiles/chutesk2streaming.json"
    ))
    .unwrap();
    // The installed shape fails at parse: `reasoning.stripFromContext`
    // accepts exactly "none" and the installed value is "all".
    let err = parse_profile_value(&chutes, "chutesk2streaming").unwrap_err();
    assert_eq!(
        err,
        "profile \"chutesk2streaming\": 'reasoning.stripFromContext' must be exactly \"none\""
    );

    // Normalizing that one value plus adding the discriminator makes the
    // typed markers fine; the unsupported `streaming` key is then the
    // (class 6) first failure.
    let mut with_discriminator = chutes.clone();
    with_discriminator["ephemeralSettings"]["reasoning.stripFromContext"] =
        serde_json::json!("none");
    with_discriminator["modelParams"]["chat_template_kwargs"] =
        serde_json::json!({"enable_thinking": true});
    let profile = parse_profile_value(&with_discriminator, "chutesk2streaming").unwrap();
    let err = crate::model::ModelConfig::from_profile_in(
        &profile,
        false,
        false,
        std::path::Path::new("."),
    )
    .unwrap_err()
    .to_string();
    assert_eq!(err, "unsupported profile setting(s): streaming");
}

/// The dsflash fixture is accepted end to end (typed markers, agreed effort,
/// discriminator) and renaming the profile file changes nothing.
#[test]
fn dsflash_fixture_is_accepted_and_rename_invariant() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/profiles/dsflash-mi300x.json"
    ))
    .unwrap();
    let mut with_inline_key = fixture.clone();
    with_inline_key["ephemeralSettings"]["auth-key"] = serde_json::json!("inline-test-key");

    for name in ["dsflash-mi300x", "renamed-profile"] {
        let profile =
            parse_profile_value(&with_inline_key, name).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(profile.chat_missing_discriminator.is_none(), "{name}");
        assert!(profile.ephemeral.unsupported.is_empty(), "{name}");
        assert!(profile.model_params.unsupported.is_empty(), "{name}");
        let config = crate::model::ModelConfig::from_profile_in(
            &profile,
            false,
            true,
            std::path::Path::new("."),
        )
        .unwrap_or_else(|e| panic!("{name}: {e}"));
        let kwargs = config
            .model_params
            .as_ref()
            .and_then(|params| params.chat_template_kwargs.as_ref())
            .unwrap_or_else(|| panic!("{name}: discriminator must survive"));
        assert!(kwargs.enable_thinking, "{name}");
        assert_eq!(
            kwargs.reasoning_effort,
            Some(crate::profile::DsflashEffort::High),
            "{name}: the agreed effort becomes the wire effort"
        );
    }
}

#[test]
fn debug_never_reveals_api_key() {
    let cfg = crate::model::ModelConfig {
        model: "m".into(),
        base_url: crate::profile::RedactedUrl::from_unvalidated("http://127.0.0.1:1/v1"),
        api_key: "sk-super-secret".into(),
        keyfile_path: None,
        max_output_tokens: Some(16384),
        timeout: Some(std::time::Duration::from_millis(900000)),
        model_params: None,
        context_limit: None,
    };
    let redacted = format!("{cfg:?}");
    assert!(
        !redacted.contains("sk-super-secret"),
        "Debug leaks the key: {redacted}"
    );
}

#[test]
fn file_profile_uses_local_keyfile_and_no_settings_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let local = dir.path().join("local.key");
    std::fs::write(&local, "sk-local\n").unwrap();
    let inner = crate::profile::Profile {
        ephemeral: crate::profile::EphemeralSettings {
            base_url: Some(crate::profile::RedactedUrl::from_unvalidated(
                "http://127.0.0.1:1/v1",
            )),
            auth_keyfile_orig: Some(local.display().to_string()),
            ..Default::default()
        },
        ..base_profile()
    };
    let cfg = crate::model::ModelConfig::from_profile(&inner, true, true).unwrap();
    assert_eq!(cfg.api_key, "sk-local");

    let inner = crate::profile::Profile {
        ephemeral: crate::profile::EphemeralSettings {
            base_url: Some(crate::profile::RedactedUrl::from_unvalidated(
                "http://127.0.0.1:1/v1",
            )),
            auth_keyfile_orig: None,
            ..Default::default()
        },
        ..base_profile()
    };
    let err = crate::model::ModelConfig::from_profile(&inner, true, true).unwrap_err();
    assert!(matches!(err, crate::model::ModelError::NoProfileAuth));
}

#[test]
fn openai_responses_uses_the_openai_settings_keyfile_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let keyfile = dir.path().join("openai.key");
    std::fs::write(&keyfile, "sk-responses\n").unwrap();
    std::fs::write(
        dir.path().join("settings.json"),
        serde_json::to_vec(&serde_json::json!({
            "providerKeyfiles": { "openai": keyfile }
        }))
        .unwrap(),
    )
    .unwrap();
    let mut profile = base_profile();
    profile.provider = "openai-responses".to_string();
    profile.ephemeral.auth_key = None;
    profile.ephemeral.auth_keyfile_orig = None;

    assert_eq!(
        super::resolve_api_key(&profile, false, dir.path()).unwrap(),
        "sk-responses"
    );
}

#[test]
fn dsflash_settings_preserved() {
    let p = base_profile();
    let inner = crate::profile::Profile {
        ephemeral: crate::profile::EphemeralSettings {
            base_url: Some(crate::profile::RedactedUrl::from_unvalidated(
                "http://127.0.0.1:1/v1",
            )),
            auth_key: Some("k".into()),
            timeout_ms: Some(900000),
            max_output_tokens: Some(16384),
            ..Default::default()
        },
        ..p
    };
    let cfg = crate::model::ModelConfig::from_profile(&inner, true, true).unwrap();
    assert_eq!(cfg.timeout, Some(std::time::Duration::from_millis(900000)));
    assert_eq!(cfg.max_output_tokens, Some(16384));
}
