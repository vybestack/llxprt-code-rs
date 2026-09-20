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
fn default_max_tool_calls_is_256() {
    // No layers: the resolved default is 256 (documented; was 16 before #15).
    let got = resolve(layers()).unwrap();
    assert_eq!(got.budgets.max_tool_calls.value, 256);
    assert_eq!(got.budgets.max_tool_calls.source, Source::Default);
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
fn request_timeout_defaults_to_900_seconds_for_all_providers() {
    let got = resolve(layers()).unwrap();
    assert_eq!(
        got.budgets.request_timeout.value,
        std::time::Duration::from_secs(900)
    );
    assert_eq!(got.budgets.request_timeout.source, Source::Default);
}

#[test]
fn request_timeout_settings_file_override_changes_resolution() {
    let mut l = layers();
    l.user_file.budgets.request_timeout = Some("120s".into());
    let got = resolve(l).unwrap();
    assert_eq!(
        got.budgets.request_timeout.value,
        std::time::Duration::from_secs(120)
    );
    assert_eq!(got.budgets.request_timeout.source, Source::UserFile);
}

#[test]
fn request_timeout_cli_beats_settings_file() {
    let mut l = layers();
    l.user_file.budgets.request_timeout = Some("120s".into());
    l.cli.budgets.request_timeout = Some("30".into());
    let got = resolve(l).unwrap();
    assert_eq!(
        got.budgets.request_timeout.value,
        std::time::Duration::from_secs(30)
    );
    assert_eq!(got.budgets.request_timeout.source, Source::Cli);
}

#[test]
fn request_timeout_profile_timeout_ms_wins_for_that_provider() {
    // The anthropic `timeout_ms` is per-provider data expressed in the profile layer;
    // it outranks the settings-file value for that provider.
    let mut l = layers();
    l.user_file.budgets.request_timeout = Some("120s".into());
    l.profile.budgets.request_timeout = Some("2500ms".into());
    let got = resolve(l).unwrap();
    assert_eq!(
        got.budgets.request_timeout.value,
        std::time::Duration::from_millis(2500)
    );
    assert_eq!(got.budgets.request_timeout.source, Source::Profile);
}

#[test]
fn request_timeout_env_beats_profile() {
    let mut l = layers();
    l.profile.budgets.request_timeout = Some("2500ms".into());
    l.env.budgets.request_timeout = Some("60".into());
    let got = resolve(l).unwrap();
    assert_eq!(
        got.budgets.request_timeout.value,
        std::time::Duration::from_secs(60)
    );
    assert_eq!(got.budgets.request_timeout.source, Source::Env);
}

#[test]
fn request_timeout_above_lease_margin_still_fails_validation() {
    // The resolver resolves the value; the lease-margin bound stays enforced on it.
    let mut l = layers();
    l.cli.budgets.request_timeout = Some("3600".into());
    let settings = resolve(l).unwrap();
    assert_eq!(
        settings.budgets.request_timeout.value,
        std::time::Duration::from_secs(3600)
    );
    let error =
        crate::limits::validate_timeout(Some(settings.budgets.request_timeout.value)).unwrap_err();
    assert!(error.contains("session lease"), "{error}");
}

#[test]
fn request_timeout_parse_errors_match_cli_style() {
    assert_eq!(
        parse_request_timeout("90s").unwrap(),
        std::time::Duration::from_secs(90)
    );
    assert_eq!(
        parse_request_timeout("90").unwrap(),
        std::time::Duration::from_secs(90)
    );
    assert_eq!(
        parse_request_timeout("2500ms").unwrap(),
        std::time::Duration::from_millis(2500)
    );
    assert_eq!(
        parse_request_timeout("5x").unwrap_err(),
        "--request-timeout needs an integer count (got 5x)"
    );
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

/// The digest admission floor defaults to the version-1 rule table's baseline (issue 125).
#[test]
fn digest_size_floor_defaults_to_the_filter_baseline() {
    let got = resolve(layers()).unwrap();
    assert_eq!(
        (
            got.budgets.digest_size_floor.value,
            got.budgets.digest_size_floor.source
        ),
        (
            u64::try_from(crate::context_ingress::filter::DEFAULT_DIGEST_SIZE_FLOOR).unwrap(),
            Source::Default
        )
    );
}

/// At least two layers of the precedence chain move the floor, and `--print-config`
/// reports the resolved value with the layer that supplied it (issue 125).
#[test]
fn digest_size_floor_layers_precedence_and_print_config_visibility() {
    let floor_layer = |value: u64| SettingsLayer {
        budgets: SettingsBudgets {
            digest_size_floor: Some(value),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut l = layers();
    l.user_file = floor_layer(2048);
    l.env = floor_layer(4096);
    l.cli = floor_layer(8192);
    let got = resolve(l).unwrap();
    assert_eq!(
        (
            got.budgets.digest_size_floor.value,
            got.budgets.digest_size_floor.source
        ),
        (8192, Source::Cli)
    );
    let wire = serde_json::to_value(&got).unwrap();
    assert_eq!(
        wire["budgets"]["digest_size_floor"],
        serde_json::json!({"value": 8192, "source": "cli"})
    );

    // Without the CLI layer the environment wins, and without it the user file does.
    let mut l = layers();
    l.user_file = floor_layer(2048);
    l.env = floor_layer(4096);
    let got = resolve(l).unwrap();
    assert_eq!(
        (
            got.budgets.digest_size_floor.value,
            got.budgets.digest_size_floor.source
        ),
        (4096, Source::Env)
    );
    let mut l = layers();
    l.user_file = floor_layer(2048);
    let got = resolve(l).unwrap();
    assert_eq!(
        (
            got.budgets.digest_size_floor.value,
            got.budgets.digest_size_floor.source
        ),
        (2048, Source::UserFile)
    );
}

/// A floor below the baseline is a typed error: the floor rides into the versioned
/// registry as a relaxation, and a tightening is refused (issue 125).
#[test]
fn digest_size_floor_below_the_baseline_is_a_typed_error() {
    let mut l = layers();
    l.cli = SettingsLayer {
        budgets: SettingsBudgets {
            digest_size_floor: Some(64),
            ..Default::default()
        },
        ..Default::default()
    };
    assert_eq!(
        resolve(l).unwrap_err().to_string(),
        "--digest-size-floor (64) must be at least the baseline floor (1024)"
    );
}

/// A floor above the per-result output cap makes digesting unreachable, so the
/// combination is refused naming both values — from the CLI flag and from a settings
/// file layer alike (issue 125).
#[test]
fn digest_size_floor_above_the_per_result_output_cap_is_refused() {
    let cap = crate::tools::output_limits::MAX_TOOL_OUTPUT_DEFAULT as u64;
    let floor_layer = SettingsLayer {
        budgets: SettingsBudgets {
            digest_size_floor: Some(cap + 1),
            ..Default::default()
        },
        ..Default::default()
    };
    let expected = format!(
        "digest-size-floor ({}) is above the per-result output cap --max-tool-output ({cap}): nothing could ever be admitted as a digest",
        cap + 1
    );
    let mut cli = layers();
    cli.cli = floor_layer.clone();
    assert_eq!(resolve(cli).unwrap_err().to_string(), expected);
    let mut file = layers();
    file.user_file = floor_layer;
    assert_eq!(resolve(file).unwrap_err().to_string(), expected);
}

/// A sub-floor `--max-tool-output` at the DEFAULT floor resolves (issue 125 cycle 2):
/// the cross-check would otherwise make every cap below 1024 unconfigurable with an
/// error naming a flag the user never set. The combination is coherent pre-feature
/// behavior: nothing is ever digested.
#[test]
fn a_sub_floor_output_cap_resolves_at_the_default_floor() {
    let mut l = layers();
    l.cli = output_caps(2048, 512, 16 * 1024 * 1024);
    let got = resolve(l).unwrap();
    assert_eq!(got.budgets.max_tool_output.value, 512);
    assert_eq!(
        (
            got.budgets.digest_size_floor.value,
            got.budgets.digest_size_floor.source
        ),
        (
            u64::try_from(crate::context_ingress::filter::DEFAULT_DIGEST_SIZE_FLOOR).unwrap(),
            Source::Default
        )
    );
}

/// The same sub-floor cap with an EXPLICIT floor above it is still refused naming both
/// values (issue 125 cycle 2): provenance gates the cross-check, not the check itself.
#[test]
fn an_explicit_floor_above_a_sub_floor_cap_is_still_refused() {
    let mut l = layers();
    l.cli = SettingsLayer {
        budgets: SettingsBudgets {
            max_tool_output: Some(512),
            digest_size_floor: Some(2048),
            ..Default::default()
        },
        ..Default::default()
    };
    assert_eq!(
        resolve(l).unwrap_err().to_string(),
        "digest-size-floor (2048) is above the per-result output cap --max-tool-output (512): nothing could ever be admitted as a digest"
    );
}

/// A raised floor changes the digest verdict for a fixed-size payload, and the override
/// stays a rule-table relaxation (never a `rule_version` redefinition of version 1).
#[test]
fn digest_floor_override_changes_the_digest_verdict_for_a_fixed_payload() {
    use crate::context_ingress::filter::{FilterRegistry, FilterRules, RuleVerdict};
    use crate::context_ingress::segment::segment;

    let payload = vec![b'x'; 2048];
    let segments = segment(&payload);
    let mut registry = FilterRegistry::new();
    assert_eq!(
        registry.verdict("read_file", &segments, payload.len()),
        RuleVerdict::Digest,
        "at the baseline floor a 2048-byte payload is bulk evidence"
    );
    let mut relaxed = FilterRules::v1();
    relaxed.version = 2;
    relaxed.size_floor = 4096;
    assert_eq!(registry.update_rules(relaxed).unwrap(), 2);
    assert_eq!(
        registry.rules_at(1).unwrap().size_floor,
        FilterRules::v1().size_floor,
        "version 1 keeps its baseline rules"
    );
    assert_ne!(
        registry.verdict("read_file", &segments, payload.len()),
        RuleVerdict::Digest,
        "raising the floor stops the fixed payload being digested"
    );
}
