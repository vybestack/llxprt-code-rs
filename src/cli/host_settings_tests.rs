use super::*;

#[test]
fn named_native_profile_preserves_fixture_bytes_and_host_policy() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("profiles")).unwrap();
    let bytes = include_bytes!("../../tests/fixtures/host-profile/astramedium-native.json");
    let path = root.path().join("profiles/astramedium.json");
    std::fs::write(&path, bytes).unwrap();
    let args = Args::try_parse_from(["llxprt-code-rs", "--profile", "astramedium"]).unwrap();
    let profile = resolve_profile(&args, root.path()).unwrap();
    assert_eq!(profile.model, "gpt-6-astra");
    assert_eq!(profile.ephemeral.context_limit, Some(300000));
    assert_eq!(profile.ephemeral.timeout_ms, None);
    assert_eq!(
        profile.ephemeral.shell_timeouts.default,
        Some(std::time::Duration::from_secs(2700))
    );
    assert_eq!(profile.ephemeral.shell_timeouts.maximum, None);
    assert_eq!(
        profile.ephemeral.host_image_resize.max_long_edge,
        Some(2048.into())
    );
    assert_eq!(
        profile.codex_settings.unwrap().reasoning_effort.as_deref(),
        Some("medium")
    );
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[test]
fn host_profile_request_budget_respects_resolver_precedence_and_separate_turn_time() {
    let root = tempfile::tempdir().unwrap();
    for provider in ["openai", "anthropic", "openai-responses"] {
        let input = serde_json::json!({"provider":provider,"model":"test","ephemeralSettings":{
            "stream-first-response-timeout-ms":1234,"image-resize.maxPixels":1572864,
            "shell-default-timeout-seconds":120,"shell-max-timeout-seconds":600
        }});
        for overridden in [false, true] {
            let mut profile = crate::profile::parse_profile_value(&input, "host").unwrap();
            let image = profile.ephemeral.host_image_resize.clone();
            let mut layers = crate::settings::SettingsLayers {
                config_root: root.path().to_path_buf(),
                ..Default::default()
            };
            layers.profile.budgets.request_timeout =
                profile.ephemeral.timeout_ms.map(|ms| format!("{ms}ms"));
            layers.cli.budgets.turn_time = Some("5s".into());
            if overridden {
                layers.cli.budgets.request_timeout = Some("2s".into());
            }
            let settings = crate::settings::resolve(layers).unwrap();
            apply_runtime_settings(&mut profile, &settings).unwrap();
            let expected = if overridden { 2000 } else { 1234 };
            assert_eq!(profile.ephemeral.timeout_ms, Some(expected));
            assert_eq!(
                settings.budgets.turn_time.value,
                Some(std::time::Duration::from_secs(5))
            );
            assert_eq!(profile.ephemeral.host_image_resize, image);
            assert_eq!(profile.ephemeral.shell_default_timeout_seconds, Some(120));
            assert_eq!(profile.ephemeral.shell_max_timeout_seconds, Some(600));
        }
    }
}
