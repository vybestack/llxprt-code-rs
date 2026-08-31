use super::*;

use serde_json::json;

/// `maxToolCallsPerPrompt` grammar on a Standard Chat target: `1..=512` and
/// `-1` are accepted, an absent key stays `Unset`, and 0, strings, floats, and
/// objects are rejected in the file's strict error style.
#[test]
fn max_tool_calls_per_prompt_grammar_standard_chat() {
    let profile = |max: Option<serde_json::Value>| {
        let mut value = json!({"provider": "openai-compatible", "model": "m"});
        let mut settings = serde_json::Map::new();
        if let Some(max) = max {
            settings.insert("maxToolCallsPerPrompt".to_string(), max);
        }
        value["ephemeralSettings"] = serde_json::Value::Object(settings);
        value
    };

    for (input, expected) in [
        (Some(json!(1)), MaxToolCalls::Limited(1)),
        (Some(json!(16)), MaxToolCalls::Limited(16)),
        (Some(json!(512)), MaxToolCalls::Limited(512)),
        (Some(json!(-1)), MaxToolCalls::Unlimited),
        (None, MaxToolCalls::Unset),
    ] {
        let parsed = parse_profile_value(&profile(input.clone()), "grammar")
            .unwrap_or_else(|error| panic!("expected {input:?} to parse: {error}"));
        assert_eq!(
            parsed.ephemeral.max_tool_calls_per_prompt, expected,
            "input {input:?}"
        );
    }

    for (input, message) in [
        (json!(0), "must be -1 or an integer from 1 through 512"),
        (json!(513), "must be -1 or an integer from 1 through 512"),
        (json!("unlimited"), "must be an integer"),
        (json!(4.5), "must be an integer"),
        (json!({"max": 16}), "must be an integer"),
    ] {
        let error = parse_profile_value(&profile(Some(input)), "grammar").unwrap_err();
        assert!(
            error.contains(message),
            "{error} does not contain '{message}'"
        );
    }
}

/// `maxToolCallsPerPrompt` is accepted on every provider target; this pins the
/// Codex strict-table path (the Standard Chat path is covered by the grammar
/// test above), including the `Unset` default when the key is absent.
#[test]
fn max_tool_calls_per_prompt_codex_target() {
    let load = || {
        let value: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/profiles/gpt56solhigh.json"
        ))
        .unwrap();
        value
    };

    let mut value = load();
    value["ephemeralSettings"]["maxToolCallsPerPrompt"] = json!(7);
    let codex = parse_profile_value(&value, "gpt56solhigh").unwrap();
    assert_eq!(
        codex.ephemeral.max_tool_calls_per_prompt,
        MaxToolCalls::Limited(7)
    );

    let mut value = load();
    value["ephemeralSettings"]["maxToolCallsPerPrompt"] = json!(-1);
    let codex = parse_profile_value(&value, "gpt56solhigh").unwrap();
    assert_eq!(
        codex.ephemeral.max_tool_calls_per_prompt,
        MaxToolCalls::Unlimited
    );

    let value = load();
    let codex = parse_profile_value(&value, "gpt56solhigh").unwrap();
    assert_eq!(
        codex.ephemeral.max_tool_calls_per_prompt,
        MaxToolCalls::Unset
    );

    let mut value = load();
    value["ephemeralSettings"]["maxToolCallsPerPrompt"] = json!(0);
    let error = parse_profile_value(&value, "gpt56solhigh").unwrap_err();
    assert!(error.contains("maxToolCallsPerPrompt"), "{error}");
}

/// Effective-budget precedence matrix: CLI flag > profile field > default 16.
#[test]
fn max_tool_calls_resolution_precedence() {
    // CLI + profile: the flag wins in both directions, including `-1` over a
    // bounded profile.
    assert_eq!(
        resolve_max_tool_calls(Some(5), MaxToolCalls::Limited(8)),
        Some(5)
    );
    assert_eq!(
        resolve_max_tool_calls(Some(-1), MaxToolCalls::Limited(8)),
        None
    );
    assert_eq!(
        resolve_max_tool_calls(Some(1), MaxToolCalls::Unlimited),
        Some(1)
    );
    // CLI only.
    assert_eq!(
        resolve_max_tool_calls(Some(512), MaxToolCalls::Unset),
        Some(512)
    );
    assert_eq!(
        resolve_max_tool_calls(Some(16), MaxToolCalls::Unset),
        Some(16)
    );
    // Profile only.
    assert_eq!(
        resolve_max_tool_calls(None, MaxToolCalls::Limited(8)),
        Some(8)
    );
    assert_eq!(resolve_max_tool_calls(None, MaxToolCalls::Unlimited), None);
    // Neither: the default of 16.
    assert_eq!(resolve_max_tool_calls(None, MaxToolCalls::Unset), Some(16));
    assert_eq!(
        resolve_max_tool_calls(None, MaxToolCalls::Unset),
        Some(DEFAULT_CALLS)
    );
    // Out-of-range CLI values are a CLI usage error; defensively they defer to
    // the profile field.
    assert_eq!(
        resolve_max_tool_calls(Some(0), MaxToolCalls::Limited(8)),
        Some(8)
    );
}
