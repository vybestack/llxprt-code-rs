use super::*;

fn profile() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../../tests/fixtures/task-profile/astra-headless.json"
    ))
    .unwrap()
}

#[test]
fn codex_preserves_configured_positive_context_limits() {
    for limit in [1_u64, 262_143, 262_144, 300_000, 1_000_000] {
        let mut value = profile();
        value["ephemeralSettings"]["context-limit"] = json!(limit);
        let parsed = parse_profile_value(&value, "astra").unwrap();
        assert_eq!(parsed.ephemeral.context_limit, Some(limit));
    }
}

#[test]
fn codex_rejects_invalid_context_limits() {
    for limit in [
        json!(0),
        json!(-1),
        json!(1.5),
        json!("300000"),
        json!(null),
        json!(true),
    ] {
        let mut value = profile();
        value["ephemeralSettings"]["context-limit"] = limit;
        assert!(parse_profile_value(&value, "astra").is_err());
    }
}

#[test]
fn codex_still_requires_an_explicit_context_limit() {
    let mut value = profile();
    value["ephemeralSettings"]
        .as_object_mut()
        .unwrap()
        .remove("context-limit");
    assert_eq!(
        parse_profile_value(&value, "astra").unwrap_err(),
        "profile \"astra\": missing required setting 'context-limit'"
    );
}
