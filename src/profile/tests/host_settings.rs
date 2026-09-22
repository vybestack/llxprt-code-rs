use super::*;

fn profiles() -> Vec<serde_json::Value> {
    vec![
        serde_json::from_str(include_str!(
            "../../../tests/fixtures/task-profile/astra-headless.json"
        ))
        .unwrap(),
        json!({"provider":"openai", "model":"test", "ephemeralSettings":{}}),
        json!({"provider":"openai", "model":"test", "ephemeralSettings":{"apiMode":"responses"}}),
        json!({"provider":"anthropic", "model":"test", "ephemeralSettings":{}}),
    ]
}

#[test]
fn host_keys_are_typed_for_every_provider_and_survive_resolution() {
    for mut value in profiles() {
        let map = value["ephemeralSettings"].as_object_mut().unwrap();
        for (key, n) in [
            ("shell-default-timeout-seconds", 180),
            ("shell-max-timeout-seconds", 540),
            ("stream-first-response-timeout-ms", 1234),
            ("image-resize.maxLongEdge", 2048),
            ("image-resize.maxShortEdge", 768),
            ("image-resize.maxPixels", 1572864),
        ] {
            map.insert(key.into(), json!(n));
        }
        let profile = parse_profile_value(&value, "astramedium").unwrap();
        let e = &profile.ephemeral;
        assert_eq!(e.shell_default_timeout_seconds, Some(180));
        assert_eq!(e.shell_max_timeout_seconds, Some(540));
        assert_eq!(e.timeout_ms, Some(1234));
        assert_eq!(e.image_resize.max_long_edge, Some(2048));
        assert_eq!(e.image_resize.max_short_edge, Some(768));
        assert_eq!(e.image_resize.max_pixels, Some(1572864));
        assert!(e.unsupported.is_empty());
    }
}

#[test]
fn every_host_key_rejects_malformed_values_for_every_provider() {
    for profile in profiles() {
        for key in crate::profile::host::KEYS {
            for bad in [
                json!(null),
                json!(false),
                json!("120"),
                json!(0),
                json!(-1),
                json!(-2),
                json!(1.5),
                json!([]),
                json!({}),
            ] {
                if [
                    "shell-max-timeout-seconds",
                    "stream-first-response-timeout-ms",
                ]
                .contains(key)
                    && bad == json!(-1)
                {
                    continue;
                }
                if *key == "stream-first-response-timeout-ms" && bad == json!(0) {
                    continue;
                }
                let mut value = profile.clone();
                value["ephemeralSettings"][key] = bad;
                assert!(parse_profile_value(&value, "test").is_err(), "{key}");
            }
        }
    }
}

#[test]
fn host_limits_reject_overflow_and_conflicts() {
    for profile in profiles() {
        for key in [
            "shell-default-timeout-seconds",
            "shell-max-timeout-seconds",
            "image-resize.maxLongEdge",
            "image-resize.maxShortEdge",
            "stream-first-response-timeout-ms",
        ] {
            let mut value = profile.clone();
            value["ephemeralSettings"][key] = json!(u64::MAX);
            assert!(parse_profile_value(&value, "test").is_err(), "{key}");
        }
        for (a, b) in [
            ("shell-default-timeout-seconds", "shell-max-timeout-seconds"),
            ("image-resize.maxShortEdge", "image-resize.maxLongEdge"),
        ] {
            let mut value = profile.clone();
            value["ephemeralSettings"][a] = json!(200);
            value["ephemeralSettings"][b] = json!(100);
            assert!(parse_profile_value(&value, "test").is_err());
        }
    }
}

#[test]
fn full_native_named_astramedium_retains_model_context_and_effort() {
    // Synthetic, credential-free named profile; never reads a user's profile.
    let mut value = profiles().remove(0);
    value["model"] = json!("gpt-6-astra");
    let map = value["ephemeralSettings"].as_object_mut().unwrap();
    map.insert("context-limit".into(), json!(300000));
    map.insert("reasoning.effort".into(), json!("medium"));
    for (key, n) in [
        ("shell-default-timeout-seconds", 120),
        ("shell-max-timeout-seconds", 600),
        ("stream-first-response-timeout-ms", 300000),
        ("image-resize.maxLongEdge", 2048),
        ("image-resize.maxShortEdge", 768),
        ("image-resize.maxPixels", 1572864),
    ] {
        map.insert(key.into(), json!(n));
    }
    let profile = parse_profile_value(&value, "astramedium").unwrap();
    assert_eq!(profile.model, "gpt-6-astra");
    assert_eq!(profile.ephemeral.context_limit, Some(300000));
    assert!(format!("{:?}", profile).contains("medium"));
    for key in [
        "image-resize.maxLongEdges",
        "shell-timeout-seconds",
        "stream-first-response-timeout",
    ] {
        let mut bad = value.clone();
        bad["ephemeralSettings"][key] = json!(120);
        assert!(parse_profile_value(&bad, "astramedium").is_err());
    }
}

#[test]
fn zero_response_timeout_selects_native_default() {
    for mut value in profiles() {
        value["ephemeralSettings"]["stream-first-response-timeout-ms"] = json!(0);
        assert_eq!(
            parse_profile_value(&value, "test")
                .unwrap()
                .ephemeral
                .timeout_ms,
            None
        );
    }
}

#[test]
fn negative_timeout_sentinels_remove_profile_caps_but_not_host_safety_budgets() {
    for mut value in profiles() {
        value["ephemeralSettings"]["shell-default-timeout-seconds"] = json!(2700);
        value["ephemeralSettings"]["shell-max-timeout-seconds"] = json!(-1);
        value["ephemeralSettings"]["stream-first-response-timeout-ms"] = json!(-1);
        let profile = parse_profile_value(&value, "astramedium").unwrap();
        assert_eq!(profile.ephemeral.shell_default_timeout_seconds, Some(2700));
        assert_eq!(
            profile.ephemeral.shell_max_timeout_seconds,
            Some(u32::MAX as u64)
        );
        assert_eq!(profile.ephemeral.timeout_ms, None);
    }
}
