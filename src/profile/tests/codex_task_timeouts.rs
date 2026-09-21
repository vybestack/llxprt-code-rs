use super::*;

fn astra_shape() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../../tests/fixtures/task-profile/astra-headless.json"
    ))
    .unwrap()
}

#[test]
fn codex_host_task_timeouts_accept_non_magic_numbers_or_omission() {
    let with_host_values = parse_profile_value(&astra_shape(), "astra-headless")
        .expect("host task timeout values must not block profile loading");

    let mut omitted = astra_shape();
    omitted["ephemeralSettings"]
        .as_object_mut()
        .unwrap()
        .remove("task-default-timeout-seconds");
    omitted["ephemeralSettings"]
        .as_object_mut()
        .unwrap()
        .remove("task-max-timeout-seconds");
    let omitted = parse_profile_value(&omitted, "astra-headless")
        .expect("omitted host task timeout values must load");

    assert_eq!(with_host_values.model, omitted.model);
    assert_eq!(
        with_host_values.model_params.max_output_tokens,
        omitted.model_params.max_output_tokens
    );
    assert_eq!(
        with_host_values.ephemeral.context_limit,
        omitted.ephemeral.context_limit
    );
    assert_eq!(
        with_host_values.ephemeral.max_output_tokens,
        omitted.ephemeral.max_output_tokens
    );
    assert_eq!(
        with_host_values.ephemeral.max_turns_per_prompt,
        omitted.ephemeral.max_turns_per_prompt
    );
    assert_eq!(
        with_host_values.ephemeral.timeout_ms,
        omitted.ephemeral.timeout_ms
    );
    assert_eq!(
        format!("{:?}", with_host_values.ephemeral),
        format!("{:?}", omitted.ephemeral),
        "host task values must not enter any resolved Rust profile setting"
    );
}

#[test]
fn codex_host_task_timeouts_require_json_numbers_but_not_rust_ranges() {
    let base = astra_shape();
    for key in ["task-default-timeout-seconds", "task-max-timeout-seconds"] {
        for value in [json!("3600"), json!(null), json!(true), json!([])] {
            let mut malformed = base.clone();
            malformed["ephemeralSettings"][key] = value;
            assert_eq!(
                parse_profile_value(&malformed, "astra-headless").unwrap_err(),
                format!("profile \"astra-headless\": '{key}' must be a number")
            );
        }

        for value in [json!(-1), json!(0), json!(1.5), json!(86_400)] {
            let mut valid = base.clone();
            valid["ephemeralSettings"][key] = value;
            parse_profile_value(&valid, "astra-headless").unwrap_or_else(|error| {
                panic!("valid host-owned task number {key} failed: {error}")
            });
        }
    }
}

#[test]
fn codex_owned_settings_remain_strict() {
    let mut context = astra_shape();
    context["ephemeralSettings"]["context-limit"] = json!(262_143);
    assert_eq!(
        parse_profile_value(&context, "astra-headless").unwrap_err(),
        "profile \"astra-headless\": Codex 'context-limit' must be 262144"
    );

    let mut stream_idle = astra_shape();
    stream_idle["ephemeralSettings"]["stream-idle-timeout-ms"] = json!(1);
    assert_eq!(
        parse_profile_value(&stream_idle, "astra-headless").unwrap_err(),
        "profile \"astra-headless\": 'stream-idle-timeout-ms' must be 0"
    );
}
