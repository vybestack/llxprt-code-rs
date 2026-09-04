use super::*;

#[test]
fn codex_disabled_tool_aliases_are_validated_and_equivalent() {
    let base: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/profiles/gpt56solhigh.json"
    ))
    .unwrap();

    // The fixture's host-side entries parse and are stored.
    let parsed = parse_profile_value(&base, "gpt56solhigh").unwrap();
    assert_eq!(
        parsed.ephemeral.disabled_tools,
        vec![
            "google_web_fetch".to_string(),
            "google_web_search".to_string()
        ]
    );

    // A byte-for-byte equal alias duplicates the list without changing it.
    let mut aliased = base.clone();
    aliased["ephemeralSettings"]["disabled-tools"] =
        json!(["google_web_fetch", "google_web_search"]);
    let parsed = parse_profile_value(&aliased, "gpt56solhigh").unwrap();
    assert_eq!(parsed.ephemeral.disabled_tools.len(), 2);

    // Differing alias content rejects.
    let mut differing = base.clone();
    differing["ephemeralSettings"]["disabled-tools"] = json!(["google_web_fetch"]);
    assert_eq!(
        parse_profile_value(&differing, "gpt56solhigh").unwrap_err(),
        "profile \"gpt56solhigh\": 'disabled-tools' must equal 'tools.disabled' exactly"
    );

    // Registered Rust tools cannot be disabled by profile.
    for tool in ["run_shell_command", "read_file", "replace"] {
        let mut registered = base.clone();
        registered["ephemeralSettings"]["tools.disabled"] = json!([tool]);
        assert_eq!(
            parse_profile_value(&registered, "gpt56solhigh").unwrap_err(),
            format!(
                "profile \"gpt56solhigh\": 'tools.disabled' cannot disable the registered Rust tool '{tool}'"
            )
        );
    }

    // An empty allowlist is no policy; a nonempty allowlist rejects with the
    // fixed unsupported-tool-policy message.
    let mut allowed_empty = base.clone();
    allowed_empty["ephemeralSettings"]["tools.allowed"] = json!([]);
    assert!(parse_profile_value(&allowed_empty, "gpt56solhigh").is_ok());
    let mut allowed_full = base.clone();
    allowed_full["ephemeralSettings"]["tools.allowed"] = json!(["google_web_fetch"]);
    assert_eq!(
        parse_profile_value(&allowed_full, "gpt56solhigh").unwrap_err(),
        "unsupported tool policy: 'tools.allowed' must be empty; nonempty allowlists are not implemented"
    );
}

#[test]
fn openai_responses_rejects_dsflash_only_settings_under_any_name() {
    let base: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/profiles/openai-responses-live.json"
    ))
    .unwrap();

    // A clean Responses profile still parses; no flag or prompt-note survives.
    let parsed = parse_profile_value(&base, "responses").unwrap();
    assert!(parsed.ephemeral.flags.is_empty());
    assert!(parsed.ephemeral.prompt_notes.is_empty());

    let mut with_dsflash_key = base.clone();
    with_dsflash_key["ephemeralSettings"]["reasoning.maxTokens"] = json!(2048);

    // The old name carve-out is gone: reasoning.maxTokens is unsupported under
    // every name, and the Responses parser names the failure identically.
    assert_eq!(
        parse_profile_value(&with_dsflash_key, "dsflash").unwrap_err(),
        "profile \"dsflash\": unsupported OpenAI Responses setting"
    );
    assert_eq!(
        parse_profile_value(&with_dsflash_key, "responses").unwrap_err(),
        "profile \"responses\": unsupported OpenAI Responses setting"
    );

    // Typed dsflash markers reach the inert gate (they are valid syntax) and are
    // named once each, in sorted order, under any profile name.
    let mut with_markers = base.clone();
    with_markers["ephemeralSettings"]["shell-replacement"] = json!(true);
    with_markers["ephemeralSettings"]["stream-idle-timeout-ms"] = json!(0);
    for name in ["dsflash", "responses"] {
        assert_eq!(
            parse_profile_value(&with_markers, name).unwrap_err(),
            format!(
                "profile \"{name}\": behavior-only setting(s) \
                 ephemeralSettings.shell-replacement, \
                 ephemeralSettings.stream-idle-timeout-ms are unsupported for \
                 OpenAI Responses"
            )
        );
    }

    // The exact Chat metadata keys stay inert-refused on Responses too: even the
    // accepted spellings (`false`, `enabled`) would be silently ignored here, so
    // the Responses path names both keys instead of accepting them.
    let mut with_chat_only = base.clone();
    with_chat_only["ephemeralSettings"]["loopDetectionEnabled"] = json!(false);
    with_chat_only["ephemeralSettings"]["streaming"] = json!("enabled");
    for name in ["dsflash", "responses"] {
        assert_eq!(
            parse_profile_value(&with_chat_only, name).unwrap_err(),
            format!(
                "profile \"{name}\": behavior-only setting(s) \
                 ephemeralSettings.loopDetectionEnabled, \
                 ephemeralSettings.streaming are unsupported for \
                 OpenAI Responses"
            )
        );
    }

    // Common exact metadata (`emojifilter: auto`) stays accepted on Responses.
    let mut with_emoji = base.clone();
    with_emoji["ephemeralSettings"]["emojifilter"] = json!("auto");
    parse_profile_value(&with_emoji, "responses").unwrap();
}

#[test]
fn codex_disabled_tools_reject_malformed_lists() {
    let base: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/profiles/gpt56solhigh.json"
    ))
    .unwrap();

    let mut malformed = base.clone();
    malformed["ephemeralSettings"]["tools.disabled"] = json!("not-an-array");
    assert!(parse_profile_value(&malformed, "gpt56solhigh")
        .unwrap_err()
        .contains("'tools.disabled' must be an array"));

    let mut non_string = base.clone();
    non_string["ephemeralSettings"]["disabled-tools"] = json!([3]);
    assert!(parse_profile_value(&non_string, "gpt56solhigh")
        .unwrap_err()
        .contains("'disabled-tools' entries must be strings"));

    let mut unbounded = base.clone();
    unbounded["ephemeralSettings"]["tools.disabled"] = json!(["x".repeat(65)]);
    assert!(parse_profile_value(&unbounded, "gpt56solhigh")
        .unwrap_err()
        .contains("bounded tool names"));
}
