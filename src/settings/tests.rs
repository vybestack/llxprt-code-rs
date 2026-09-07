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
    assert_eq!(
        (
            got.budgets.max_shell_output.value,
            got.budgets.max_shell_output.source
        ),
        (
            u64::try_from(crate::tools::output_limits::MAX_SHELL_OUTPUT_DEFAULT).unwrap(),
            Source::Default
        )
    );
    assert_eq!(
        (
            got.budgets.max_tool_output.value,
            got.budgets.max_turn_output.value
        ),
        (
            u64::try_from(crate::limits::MAX_TURN_OUTPUT_BYTES).unwrap(),
            u64::try_from(crate::limits::MAX_TURN_OUTPUT_BYTES).unwrap()
        )
    );
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
fn output_caps(shell: u64, tool: u64, turn: u64) -> SettingsLayer {
    SettingsLayer {
        budgets: SettingsBudgets {
            max_shell_output: Some(shell),
            max_tool_output: Some(tool),
            max_turn_output: Some(turn),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn cli_max_shell_output_flag_raises_the_shell_cap() {
    use clap::Parser as _;
    let args =
        crate::cli::Args::try_parse_from(["llxprt-code-rs", "--max-shell-output", "1048576"])
            .unwrap();
    let mut l = layers();
    l.cli = SettingsLayer {
        budgets: SettingsBudgets {
            max_shell_output: args.max_shell_output,
            ..Default::default()
        },
        ..Default::default()
    };
    let got = resolve(l).unwrap();
    assert_eq!(
        (
            got.budgets.max_shell_output.value,
            got.budgets.max_shell_output.source
        ),
        (1048576, Source::Cli)
    );
    assert_eq!(got.budgets.max_tool_output.source, Source::Default);
    assert_eq!(got.budgets.max_turn_output.source, Source::Default);
}

#[test]
fn user_file_output_caps_apply_without_the_flag() {
    let mut l = layers();
    l.user_file = output_caps(2048, 4 * 1024 * 1024, 8 * 1024 * 1024);
    let got = resolve(l).unwrap();
    assert_eq!(
        (
            got.budgets.max_shell_output.value,
            got.budgets.max_shell_output.source
        ),
        (2048, Source::UserFile)
    );
    assert_eq!(
        (
            got.budgets.max_tool_output.value,
            got.budgets.max_turn_output.value
        ),
        (4 * 1024 * 1024, 8 * 1024 * 1024)
    );
}

#[test]
fn cli_output_cap_flag_overrides_the_user_file() {
    let mut l = layers();
    l.user_file = output_caps(2048, 4 * 1024 * 1024, 8 * 1024 * 1024);
    l.cli = SettingsLayer {
        budgets: SettingsBudgets {
            max_shell_output: Some(1048576),
            ..Default::default()
        },
        ..Default::default()
    };
    let got = resolve(l).unwrap();
    assert_eq!(
        (
            got.budgets.max_shell_output.value,
            got.budgets.max_shell_output.source
        ),
        (1048576, Source::Cli)
    );
    assert_eq!(got.budgets.max_tool_output.source, Source::UserFile);
    assert_eq!(got.budgets.max_turn_output.source, Source::UserFile);
}

#[test]
fn per_tool_cap_above_turn_cap_is_a_typed_error() {
    let mut l = layers();
    l.cli = output_caps(32 * 1024, 40_000_000, 16 * 1024 * 1024);
    assert_eq!(
        resolve(l).unwrap_err().to_string(),
        "--max-tool-output (40000000) exceeds --max-turn-output (16777216)"
    );
}

#[test]
fn shell_cap_above_turn_cap_is_a_typed_error() {
    let mut l = layers();
    l.env = output_caps(40_000_000, 16 * 1024 * 1024, 16 * 1024 * 1024);
    assert_eq!(
        resolve(l).unwrap_err().to_string(),
        "--max-shell-output (40000000) exceeds --max-turn-output (16777216)"
    );
}

#[test]
fn raised_turn_cap_admits_larger_per_result_caps() {
    let mut l = layers();
    l.cli = output_caps(64 * 1024 * 1024, 32 * 1024 * 1024, 64 * 1024 * 1024);
    let got = resolve(l).unwrap();
    assert_eq!(got.budgets.max_turn_output.value, 64 * 1024 * 1024);
    assert_eq!(got.budgets.max_shell_output.value, 64 * 1024 * 1024);
}

#[test]
fn print_config_wire_carries_resolved_output_caps() {
    let mut l = layers();
    l.cli = SettingsLayer {
        budgets: SettingsBudgets {
            max_shell_output: Some(1048576),
            ..Default::default()
        },
        ..Default::default()
    };
    let wire = serde_json::to_value(resolve(l).unwrap()).unwrap();
    assert_eq!(
        wire["budgets"]["max_shell_output"],
        serde_json::json!({"value": 1048576, "source": "cli"})
    );
    assert_eq!(
        wire["budgets"]["max_tool_output"],
        serde_json::json!({
            "value": u64::try_from(crate::limits::MAX_TURN_OUTPUT_BYTES).unwrap(),
            "source": "default"
        })
    );
    assert_eq!(
        wire["budgets"]["max_turn_output"],
        serde_json::json!({
            "value": u64::try_from(crate::limits::MAX_TURN_OUTPUT_BYTES).unwrap(),
            "source": "default"
        })
    );
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
