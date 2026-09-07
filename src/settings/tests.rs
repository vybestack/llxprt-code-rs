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
        tools: SettingsTools::default(),
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
fn unknown_settings_json_key_is_error() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("settings.json"), r#"{"unknown": true}"#).unwrap();
    assert!(load_user_file(temp.path())
        .unwrap_err()
        .contains("unknown field"));
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

fn caps(tool: Option<usize>, shell: Option<usize>) -> SettingsLayer {
    SettingsLayer {
        tools: SettingsTools {
            max_tool_output: tool,
            max_shell_output: shell,
        },
        ..Default::default()
    }
}

#[test]
fn tool_output_caps_default_to_the_builtin_constants() {
    let got = resolve(layers()).unwrap();
    assert_eq!(
        got.tools.max_tool_output.value,
        crate::tools::output_limits::MAX_TOOL_OUTPUT_DEFAULT
    );
    assert_eq!(
        got.tools.max_shell_output.value,
        crate::tools::output_limits::MAX_SHELL_OUTPUT_DEFAULT
    );
    assert_eq!(got.tools.max_tool_output.source, Source::Default);
    assert_eq!(got.tools.max_shell_output.source, Source::Default);
}

#[test]
fn tool_output_caps_follow_the_standard_precedence() {
    let mut l = layers();
    l.user_file = caps(Some(1 << 20), None);
    let got = resolve(l).unwrap();
    assert_eq!(got.tools.max_tool_output.value, 1 << 20);
    assert_eq!(got.tools.max_tool_output.source, Source::UserFile);
    assert_eq!(
        got.tools.max_shell_output.value,
        crate::tools::output_limits::MAX_SHELL_OUTPUT_DEFAULT
    );

    let mut l = layers();
    l.user_file = caps(Some(1 << 20), Some(1 << 18));
    l.env = caps(None, Some(1 << 17));
    l.cli = caps(Some(1 << 22), None);
    let got = resolve(l).unwrap();
    assert_eq!(
        (
            got.tools.max_tool_output.value,
            got.tools.max_tool_output.source
        ),
        (1 << 22, Source::Cli)
    );
    assert_eq!(
        (
            got.tools.max_shell_output.value,
            got.tools.max_shell_output.source
        ),
        (1 << 17, Source::Env)
    );
}

#[test]
fn tool_output_caps_reject_absurd_values() {
    let ceiling = crate::tools::output_limits::MAX_TOOL_OUTPUT_DEFAULT;
    for (flag, value) in [
        ("--max-tool-output", 0usize),
        ("--max-tool-output", ceiling + 1),
        ("--max-shell-output", 0),
        ("--max-shell-output", ceiling + 1),
    ] {
        let mut l = layers();
        if flag == "--max-tool-output" {
            l.env = caps(Some(value), None);
        } else {
            l.env = caps(None, Some(value));
        }
        let error = resolve(l).unwrap_err().to_string();
        assert!(
            error.starts_with(&format!(
                "{flag} must be an integer from 1 through {ceiling} bytes (got {value})"
            )),
            "{error}"
        );
    }
}

#[test]
fn runtime_output_limits_follow_resolver() {
    let mut l = layers();
    l.user_file = caps(Some(1 << 20), Some(1 << 15));
    l.cli = caps(Some(1 << 21), None);
    let settings = resolve(l).unwrap();
    assert_eq!(settings.tools.max_tool_output.value, 1 << 21);
    assert_eq!(settings.tools.max_tool_output.source, Source::Cli);
    assert_eq!(settings.tools.max_shell_output.value, 1 << 15);
    assert_eq!(settings.tools.max_shell_output.source, Source::UserFile);
}
