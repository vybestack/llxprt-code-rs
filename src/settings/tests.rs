use super::*;
use std::path::PathBuf;

fn layers() -> SettingsLayers {
    SettingsLayers {
        config_root: PathBuf::from("/config"),
        ..Default::default()
    }
}
fn budget(n: i64) -> SettingsLayer {
    SettingsLayer {
        budgets: SettingsBudgets {
            max_tool_calls: Some(n),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn precedence_defaults_only() {
    let got = resolve(layers()).unwrap();
    assert_eq!(got.provider.base_url.source, Source::Default);
    assert_eq!(got.provider.model.source, Source::Default);
    assert_eq!(got.budgets.max_tool_calls.source, Source::Default);
    assert_eq!(got.budgets.turn_time.source, Source::Default);
    assert_eq!(got.paths.config_root.source, Source::Default);
}
#[test]
fn precedence_user_file_overrides_default() {
    let mut l = layers();
    l.user_file = budget(7);
    assert_eq!(
        resolve(l).unwrap().budgets.max_tool_calls.source,
        Source::UserFile
    );
}
#[test]
fn precedence_profile_overrides_user_file() {
    let mut l = layers();
    l.user_file = budget(7);
    l.profile = budget(8);
    let got = resolve(l).unwrap();
    assert_eq!(
        (
            got.budgets.max_tool_calls.value,
            got.budgets.max_tool_calls.source
        ),
        (8, Source::Profile)
    );
}
#[test]
fn precedence_env_overrides_profile() {
    let mut l = layers();
    l.profile = budget(8);
    l.env = budget(9);
    let got = resolve(l).unwrap();
    assert_eq!(
        (
            got.budgets.max_tool_calls.value,
            got.budgets.max_tool_calls.source
        ),
        (9, Source::Env)
    );
}
#[test]
fn precedence_cli_overrides_env() {
    let mut l = layers();
    l.env = budget(9);
    l.cli = budget(10);
    let got = resolve(l).unwrap();
    assert_eq!(
        (
            got.budgets.max_tool_calls.value,
            got.budgets.max_tool_calls.source
        ),
        (10, Source::Cli)
    );
}
#[test]
fn unresolved_layer_defers_downward() {
    let mut l = layers();
    l.user_file = SettingsLayer {
        provider: SettingsProvider {
            model: Some("user-model".into()),
            ..Default::default()
        },
        budgets: SettingsBudgets {
            turn_time: Some("2h".into()),
            ..Default::default()
        },
        paths: SettingsPaths {
            config_root: Some(PathBuf::from("/user-config")),
        },
    };
    l.profile = SettingsLayer {
        provider: SettingsProvider {
            base_url: Some("https://profile.invalid/v1".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    l.env = budget(11);
    let got = resolve(l).unwrap();
    assert_eq!(got.provider.model.source, Source::UserFile);
    assert_eq!(got.provider.base_url.source, Source::Profile);
    assert_eq!(got.budgets.max_tool_calls.source, Source::Env);
    assert_eq!(got.budgets.turn_time.source, Source::UserFile);
    assert_eq!(got.paths.config_root.source, Source::UserFile);
}
#[test]
/// An unknown key **inside** a section the resolver owns is still a settings-load error
/// (issue 202 moved tolerance to the shared top level only, never inside owned data).
fn unknown_owned_section_key_is_error() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("settings.json"),
        r#"{"ui": {"theme": "Green Screen"}, "provider": {"unknown": true}}"#,
    )
    .unwrap();
    let error = load_user_file(temp.path()).unwrap_err();
    assert!(error.contains("unknown field `unknown`"), "{error}");
    assert!(
        error.contains("expected one of `base_url`, `model`, `profile_path`"),
        "{error}"
    );
}

/// Regression (issue 202): `settings.json` is a shared multi-tool surface. The
/// TypeScript llxprt-code app legitimately writes its own top-level keys (`ui`,
/// `oauthEnabledProviders`, `providerKeyfiles`, ...), so an unknown top-level sibling
/// must be ignored, not fail the load. The same read still applies every owned section
/// beside the sibling key.
#[test]
fn shared_user_file_sibling_keys_are_ignored_but_owned_sections_apply() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("settings.json"),
        r#"{"ui": {"theme": "Green Screen"},
            "oauthEnabledProviders": {"codex": true},
            "providerKeyfiles": {"openai": "/no/such/keyfile"},
            "budgets": {"max_tool_calls": 9}}"#,
    )
    .unwrap();
    let layer = load_user_file(temp.path()).unwrap();
    assert_eq!(layer.budgets.max_tool_calls, Some(9));
    assert_eq!(layer.provider.base_url, None, "sibling keys add nothing");
}

/// Strictness stays inside every section the resolver owns: a misspelled owned key is
/// still a settings-load failure with the section's field list, so a typo cannot be
/// silently swallowed by the tolerant root.
#[test]
fn owned_sections_stay_strict_on_misspelled_keys() {
    let temp = tempfile::tempdir().unwrap();
    for (body, expected_field) in [
        (r#"{"budgets": {"maxToolCalls": 4}}"#, "maxToolCalls"),
        (r#"{"paths": {"configRoot": "/tmp"}}"#, "configRoot"),
    ] {
        std::fs::write(temp.path().join("settings.json"), body).unwrap();
        let error = load_user_file(temp.path()).unwrap_err();
        assert!(
            error.contains(&format!("unknown field `{expected_field}`")),
            "expected an unknown-field failure for {body}, got: {error}"
        );
    }
}

/// Strictness stays inside every owned section for wrong types, duplicate keys, and
/// invalid values: the settings-load (or settings-resolve) classification is unchanged.
#[test]
fn owned_sections_stay_strict_on_types_duplicates_and_values() {
    let temp = tempfile::tempdir().unwrap();
    // Wrong type inside an owned section.
    std::fs::write(
        temp.path().join("settings.json"),
        r#"{"ui": {"theme": "Green Screen"}, "provider": {"model": 7}}"#,
    )
    .unwrap();
    let error = load_user_file(temp.path()).unwrap_err();
    assert!(
        error.contains("invalid type: integer `7`, expected a string"),
        "wrong type inside an owned section must fail: {error}"
    );

    // A duplicate owned key is still rejected, beside an ignored sibling key.
    std::fs::write(
        temp.path().join("settings.json"),
        r#"{"oauthEnabledProviders": {"codex": true},
            "budgets": {"max_tool_calls": 7, "max_tool_calls": 8}}"#,
    )
    .unwrap();
    let error = load_user_file(temp.path()).unwrap_err();
    assert!(
        error.contains("duplicate field `max_tool_calls`"),
        "a duplicate owned key must fail: {error}"
    );

    // An invalid owned value fails in the resolver, after the load succeeds.
    std::fs::write(
        temp.path().join("settings.json"),
        r#"{"ui": {"theme": "Green Screen"}, "budgets": {"max_tool_calls": 0}}"#,
    )
    .unwrap();
    let mut l = layers();
    l.user_file = load_user_file(temp.path()).unwrap();
    let error = resolve(l).unwrap_err().to_string();
    assert!(
        error.starts_with("--max-tool-calls must be -1 or an integer from 1 through 512 (got "),
        "an invalid owned value must fail in the resolver: {error}"
    );
}

#[test]
fn env_max_tool_calls_validated_like_cli() {
    for value in [0, 513] {
        let mut l = layers();
        l.env = budget(value);
        let error = resolve(l).unwrap_err().to_string();
        assert!(
            error.starts_with("--max-tool-calls must be -1 or an integer from 1 through 512 (got ")
        );
    }
}
#[test]
fn settings_schema_drift() {
    let wire = serde_json::to_value(SettingsLayer::default()).unwrap();
    let object = wire.as_object().unwrap();
    assert!(object.is_empty());
    let wire = serde_json::to_value(SettingsLayer {
        provider: SettingsProvider {
            base_url: Some("http://loopback.invalid/v1".into()),
            ..Default::default()
        },
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        wire,
        serde_json::json!({"provider": {"base_url": "http://loopback.invalid/v1"}})
    );
    assert_eq!(SETTINGS_SCHEMA_ID, "https://llxprt.dev/schema/settings-v1");
}

#[test]
fn runtime_max_tool_calls_follows_resolver() {
    let mut l = layers();
    l.user_file = budget(4);
    l.profile = budget(8);
    l.env = budget(12);
    l.cli = budget(16);
    let settings = resolve(l).unwrap();
    assert_eq!(
        (
            settings.budgets.max_tool_calls.value,
            settings.budgets.max_tool_calls.source
        ),
        (16, Source::Cli)
    );
}

#[test]
fn runtime_settings_json_applies_when_unoverridden() {
    let mut l = layers();
    l.user_file = SettingsLayer {
        budgets: SettingsBudgets {
            max_tool_calls: Some(23),
            ..Default::default()
        },
        ..Default::default()
    };
    let settings = resolve(l).unwrap();
    assert_eq!(
        (
            settings.budgets.max_tool_calls.value,
            settings.budgets.max_tool_calls.source
        ),
        (23, Source::UserFile)
    );
}

#[test]
fn runtime_env_overrides_profile_for_turn_time() {
    let mut l = layers();
    l.profile.budgets.turn_time = Some("2h".into());
    l.env.budgets.turn_time = Some("90s".into());
    let settings = resolve(l).unwrap();
    assert_eq!(
        settings.budgets.turn_time.value,
        Some(std::time::Duration::from_secs(90))
    );
    assert_eq!(settings.budgets.turn_time.source, Source::Env);
}

#[test]
fn cli_flag_error_messages_unchanged() {
    assert_eq!(
        validate_max_tool_calls(0).unwrap_err(),
        "--max-tool-calls must be -1 or an integer from 1 through 512 (got 0)"
    );
    assert_eq!(
        parse_turn_time("5x").unwrap_err(),
        "--turn-time unit must be s, m, or h (got 5x)"
    );
}

#[test]
fn no_settings_env_reads_outside_resolver() {
    // Runtime accepts Settings, so resolved values are the sole budget inputs to build_agent.
    let mut l = layers();
    l.env = budget(31);
    let settings = resolve(l).unwrap();
    assert_eq!(settings.budgets.max_tool_calls.value, 31);
    assert_eq!(settings.budgets.max_tool_calls.source, Source::Env);
}

#[test]
fn settings_serialize_round_trips_through_strict_loader() {
    let expected = SettingsLayer {
        provider: SettingsProvider {
            base_url: Some("http://127.0.0.1:8080/v1".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("settings.json"),
        serde_json::to_vec(&expected).unwrap(),
    )
    .unwrap();
    assert_eq!(load_user_file(temp.path()).unwrap(), expected);
}
