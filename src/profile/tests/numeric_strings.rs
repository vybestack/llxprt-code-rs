use super::*;

/// Numeric strings are coerced for numeric sampling settings: the TS llxprt-code
/// accepts `modelParams.top_p: ".95"` (crusoeglm.json) and `temperature: "1"`, so
/// the Rust parser accepts the same numeric-string spellings instead of failing
/// profile-load. Padded, junk-suffixed, and non-finite spellings stay wrong types.
#[test]
fn numeric_strings_are_coerced_for_sampling_settings() {
    for (alias, temperature, top_p) in [
        ("temperature", Some(0.95_f64), None),
        ("temperature", Some(1.0), None),
        ("top_p", None, Some(0.95)),
        ("topP", None, Some(0.8)),
    ] {
        let literal = match (temperature, top_p) {
            (Some(0.95), _) | (_, Some(0.95)) => ".95",
            (Some(1.0), _) => "1",
            (_, Some(0.8)) => "0.8",
            _ => unreachable!("table rows stay literal"),
        };
        let profile = parse_profile_value(
            &json!({"provider": "openai", "model": "m", "modelParams": {alias: literal}}),
            "coerced",
        )
        .unwrap_or_else(|err| panic!("{alias} numeric string must parse: {err}"));
        assert_eq!(profile.model_params.temperature, temperature, "{alias}");
        assert_eq!(profile.model_params.top_p, top_p, "{alias}");
    }
    for (key, value) in [
        ("temperature", json!("hot")),
        ("top_p", json!("")),
        ("top_p", json!(" 0.9 ")),
        ("top_p", json!("0.9junk")),
        ("top_p", json!("NaN")),
        ("top_p", json!("inf")),
    ] {
        let err = parse_profile_value(
            &json!({"provider": "openai", "model": "m", "modelParams": {key: value}}),
            "bad",
        )
        .expect_err("a non-numeric string is a wrong type, never a coerced value");
        assert_eq!(
            err,
            format!("profile \"bad\": '{key}' must be a number"),
            "{key}: {value}"
        );
    }
}

/// The installed `crusoeglm.json` fixture carries `modelParams.top_p` as the string
/// `".95"`; it must parse offline so the runtime matches the TS llxprt-code
/// disposition instead of failing profile-load. The staged synthetic variant carries
/// `top_p: "0.8"` beside the dsflash discriminator, so both variants coerce.
#[test]
fn crusoeglm_fixture_numeric_string_top_p_loads() {
    for (file, text, temperature, top_p) in [
        (
            "crusoeglm.json",
            include_str!("../../../tests/fixtures/profiles/crusoeglm.json"),
            1.0_f64,
            0.95,
        ),
        (
            "crusoeglm.without-prompt-caching.synthetic.json",
            include_str!(
                "../../../tests/fixtures/profiles/crusoeglm.without-prompt-caching.synthetic.json"
            ),
            0.3,
            0.8,
        ),
    ] {
        let value: serde_json::Value = serde_json::from_str(text)
            .unwrap_or_else(|err| panic!("{file}: fixture must be valid JSON: {err}"));
        let profile = parse_profile_value(&value, "crusoeglm")
            .unwrap_or_else(|err| panic!("{file} must load offline: {err}"));
        assert_eq!(
            profile.model_params.temperature,
            Some(temperature),
            "{file}"
        );
        assert_eq!(profile.model_params.top_p, Some(top_p), "{file}");
    }
}

/// OpenAI Responses parses the same numeric sampling settings through
/// `finite_number`, so a numeric string such as `".95"` loads there too instead
/// of failing profile-load on the target that owns its own sampling keys.
#[test]
fn openai_responses_coerces_numeric_sampling_strings() {
    let profile = parse_profile_value(
        &json!({
            "provider": "openai",
            "model": "gpt-5.6",
            "ephemeralSettings": {
                "apiMode": "responses",
                "base-url": "http://127.0.0.1:8080/v1/responses",
                "reasoning.enabled": true,
                "reasoning.effort": "high",
                "reasoning.summary": "auto"
            },
            "modelParams": {"temperature": "1", "top_p": ".95"}
        }),
        "responses-coerced",
    )
    .unwrap();
    assert_eq!(profile.model_params.temperature, Some(1.0));
    assert_eq!(profile.model_params.top_p, Some(0.95));

    for (key, value) in [("temperature", json!("hot")), ("top_p", json!("NaN"))] {
        let err = parse_profile_value(
            &json!({
                "provider": "openai",
                "model": "gpt-5.6",
                "ephemeralSettings": {
                    "apiMode": "responses",
                    "base-url": "http://127.0.0.1:8080/v1/responses",
                    "reasoning.enabled": true,
                    "reasoning.effort": "high",
                    "reasoning.summary": "auto"
                },
                "modelParams": {key: value}
            }),
            "responses-bad",
        )
        .expect_err("a non-numeric string stays a wrong type on Responses");
        assert_eq!(
            err,
            format!("profile \"responses-bad\": '{key}' must be a finite number"),
            "{key}: {value}"
        );
    }
}
