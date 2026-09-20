use super::*;

fn astra() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../../tests/fixtures/task-profile/astramedium.json"
    ))
    .unwrap()
}

#[test]
fn configured_codex_context_budgets_are_preserved() {
    for limit in [1, 262_143, 262_144, 300_000, u64::MAX] {
        let mut value = astra();
        value["ephemeralSettings"]["context-limit"] = json!(limit);
        let profile = parse_profile_value(&value, "astramedium").unwrap();
        assert_eq!(profile.ephemeral.context_limit, Some(limit));
    }
}

#[test]
fn codex_context_budget_rejects_zero_and_wrong_types() {
    for invalid in [
        json!(0),
        json!(-1),
        json!(1.5),
        json!("300000"),
        json!(true),
        json!(null),
        json!([]),
        json!({}),
    ] {
        let mut value = astra();
        value["ephemeralSettings"]["context-limit"] = invalid;
        let error = parse_profile_value(&value, "astramedium").unwrap_err();
        assert!(error.contains("context-limit"), "{error}");
    }
}

#[test]
fn codex_reasoning_preserves_valid_efforts_and_rejects_invalid_values() {
    for effort in ["low", "medium", "high"] {
        let mut value = astra();
        value["ephemeralSettings"]["reasoning.effort"] = json!(effort);
        let profile = parse_profile_value(&value, "astramedium").unwrap();
        assert_eq!(
            profile.codex_settings.unwrap().reasoning_effort.as_deref(),
            Some(effort)
        );
    }
    for invalid in [
        json!("automatic"),
        json!(""),
        json!(1),
        json!(true),
        json!(null),
        json!([]),
        json!({}),
    ] {
        let mut value = astra();
        value["ephemeralSettings"]["reasoning.effort"] = invalid;
        assert!(parse_profile_value(&value, "astramedium")
            .unwrap_err()
            .contains("reasoning.effort"));
    }
    let mut value = astra();
    value["ephemeralSettings"]["reasoning.enabled"] = json!(false);
    assert!(parse_profile_value(&value, "astramedium").is_err());
    let settings = value["ephemeralSettings"].as_object_mut().unwrap();
    settings.remove("reasoning.effort");
    settings.remove("reasoning.summary");
    assert_eq!(
        parse_profile_value(&value, "astramedium")
            .unwrap()
            .codex_settings
            .unwrap()
            .reasoning_effort,
        None
    );
}

#[test]
fn astramedium_host_settings_are_typed_inert_values() {
    let value = astra();
    let profile = parse_profile_value(&value, "astramedium").unwrap();
    let mut without_host = value.clone();
    for key in [
        "image-resize.maxLongEdge",
        "image-resize.maxShortEdge",
        "image-resize.maxPixels",
        "shell-default-timeout-seconds",
        "shell-max-timeout-seconds",
        "stream-first-response-timeout-ms",
    ] {
        without_host["ephemeralSettings"]
            .as_object_mut()
            .unwrap()
            .remove(key);
        for invalid in [
            json!("2048"),
            json!(null),
            json!(false),
            json!([]),
            json!({}),
        ] {
            let mut malformed = value.clone();
            malformed["ephemeralSettings"][key] = invalid;
            assert!(parse_profile_value(&malformed, "astramedium")
                .unwrap_err()
                .contains(key));
        }
    }
    let omitted = parse_profile_value(&without_host, "astramedium").unwrap();
    assert_eq!(
        format!("{:?}", profile.ephemeral),
        format!("{:?}", omitted.ephemeral)
    );
    assert_eq!(
        format!("{:?}", profile.model_params),
        format!("{:?}", omitted.model_params)
    );
    assert_eq!(profile.codex_settings, omitted.codex_settings);
    assert_eq!(profile.ephemeral.timeout_ms, None);
    for active in [0, 1, 30000, -2] {
        let mut invalid = value.clone();
        invalid["ephemeralSettings"]["stream-first-response-timeout-ms"] = json!(active);
        assert!(parse_profile_value(&invalid, "astramedium")
            .unwrap_err()
            .contains("must be -1"));
    }
}
