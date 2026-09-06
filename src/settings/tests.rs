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
        assert!(error.starts_with("--max-tool-calls must be in the range 1..=512 (got "));
    }
}
#[test]
fn settings_schema_drift() {
    let wire = serde_json::to_value(SettingsLayer::default()).unwrap();
    let object = wire.as_object().unwrap();
    assert_eq!(
        object.keys().map(String::as_str).collect::<Vec<_>>(),
        ["budgets", "paths", "provider"]
    );
    for (section, keys) in [
        ("provider", ["base_url", "model", "profile_path"].as_slice()),
        ("budgets", ["max_tool_calls", "turn_time"].as_slice()),
        ("paths", ["config_root"].as_slice()),
    ] {
        assert_eq!(
            object[section]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            keys
        );
    }
    assert_eq!(SETTINGS_SCHEMA_ID, "https://llxprt.dev/schema/settings-v1");
}
