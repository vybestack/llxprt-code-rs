use super::*;

fn profiles() -> Vec<serde_json::Value> {
    vec![
        json!({"provider":"openai", "model":"test", "ephemeralSettings":{}}),
        json!({"provider":"openai", "model":"test", "ephemeralSettings":{"apiMode":"responses"}}),
        json!({"provider":"anthropic", "model":"test", "ephemeralSettings":{}}),
    ]
}

const IMAGE_KEYS: [&str; 3] = [
    "image-resize.maxLongEdge",
    "image-resize.maxShortEdge",
    "image-resize.maxPixels",
];

#[test]
fn non_codex_host_keys_are_typed_and_retained() {
    for mut value in profiles() {
        for (key, n) in [
            ("shell-default-timeout-seconds", 180),
            ("shell-max-timeout-seconds", 540),
            ("stream-first-response-timeout-ms", 1234),
            ("image-resize.maxLongEdge", 2048),
            ("image-resize.maxShortEdge", 768),
            ("image-resize.maxPixels", 1572864),
        ] {
            value["ephemeralSettings"][key] = json!(n);
        }
        let profile = parse_profile_value(&value, "host").unwrap();
        let e = &profile.ephemeral;
        assert_eq!(e.shell_default_timeout_seconds, Some(180));
        assert_eq!(e.shell_max_timeout_seconds, Some(540));
        assert_eq!(e.timeout_ms, Some(1234));
        assert_eq!(e.host_image_resize.max_long_edge, Some(2048.into()));
        assert_eq!(e.host_image_resize.max_short_edge, Some(768.into()));
        assert_eq!(e.host_image_resize.max_pixels, Some(1572864.into()));
        assert!(e.unsupported.is_empty());
    }
}

#[test]
fn non_codex_image_limits_reject_wrong_types_zero_negative_and_overflow() {
    for profile in profiles() {
        for key in IMAGE_KEYS {
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
                let mut value = profile.clone();
                value["ephemeralSettings"][key] = bad;
                assert!(parse_profile_value(&value, "host")
                    .unwrap_err()
                    .contains(key));
            }
            let mut value = profile.clone();
            value["ephemeralSettings"][key] = json!(u64::MAX);
            if key == "image-resize.maxPixels" {
                assert_eq!(
                    parse_profile_value(&value, "host")
                        .unwrap()
                        .ephemeral
                        .host_image_resize
                        .max_pixels,
                    Some(u64::MAX.into())
                );
            } else {
                assert!(parse_profile_value(&value, "host")
                    .unwrap_err()
                    .contains(key));
                value["ephemeralSettings"][key] = json!(u32::MAX);
                assert!(parse_profile_value(&value, "host").is_ok());
            }
        }
        let mut conflict = profile.clone();
        conflict["ephemeralSettings"]["image-resize.maxLongEdge"] = json!(100);
        conflict["ephemeralSettings"]["image-resize.maxShortEdge"] = json!(200);
        assert!(parse_profile_value(&conflict, "host")
            .unwrap_err()
            .contains("short edge"));
        conflict["ephemeralSettings"]["image-resize.maxShortEdge"] = json!(100);
        assert!(parse_profile_value(&conflict, "host").is_ok());
        let omitted = parse_profile_value(&profile, "host").unwrap();
        assert_eq!(
            omitted.ephemeral.host_image_resize,
            HostImageResizeSettings::default()
        );
    }
}

#[test]
fn non_codex_request_budget_sentinels_and_bounds_preserve_native_safety() {
    let key = "stream-first-response-timeout-ms";
    for profile in profiles() {
        for valid in [json!(-1), json!(0)] {
            let mut value = profile.clone();
            value["ephemeralSettings"][key] = valid;
            assert_eq!(
                parse_profile_value(&value, "host")
                    .unwrap()
                    .ephemeral
                    .timeout_ms,
                None
            );
        }
        for valid in [1, 1234, 300000] {
            let mut value = profile.clone();
            value["ephemeralSettings"][key] = json!(valid);
            assert_eq!(
                parse_profile_value(&value, "host")
                    .unwrap()
                    .ephemeral
                    .timeout_ms,
                Some(valid)
            );
        }
        for bad in [
            json!(-2),
            json!(null),
            json!(false),
            json!("120"),
            json!(1.5),
            json!([]),
            json!({}),
            json!(u64::MAX),
        ] {
            let mut value = profile.clone();
            value["ephemeralSettings"][key] = bad;
            assert!(parse_profile_value(&value, "host").is_err());
        }
    }
}

#[test]
fn current_main_provider_specific_host_contracts_are_not_unified() {
    let base: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/host-profile/astramedium-native.json"
    ))
    .unwrap();
    let codex = parse_profile_value(&base, "astramedium").unwrap();
    assert_eq!(
        codex.ephemeral.shell_timeouts.default,
        Some(std::time::Duration::from_secs(2700))
    );
    assert_eq!(codex.ephemeral.shell_timeouts.maximum, None);
    assert_eq!(codex.ephemeral.timeout_ms, None);
    for n in [json!(0), json!(1234)] {
        let mut value = base.clone();
        value["ephemeralSettings"]["stream-first-response-timeout-ms"] = n;
        assert!(parse_profile_value(&value, "astramedium").is_err());
    }
    let mut numeric_host_image = base;
    numeric_host_image["ephemeralSettings"]["image-resize.maxLongEdge"] = json!(-1.5);
    assert!(parse_profile_value(&numeric_host_image, "astramedium").is_ok());
    for profile in profiles() {
        for key in ["shell-default-timeout-seconds", "shell-max-timeout-seconds"] {
            for bad in [json!(-1), json!(0), json!(7201)] {
                let mut value = profile.clone();
                value["ephemeralSettings"][key] = bad;
                assert!(parse_profile_value(&value, "host")
                    .unwrap_err()
                    .contains(key));
            }
        }
    }
}
