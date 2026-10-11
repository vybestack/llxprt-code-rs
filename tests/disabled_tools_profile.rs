//! Removed tool spelling must reach each loader's ordinary unknown-key refusal,
//! before any provider request. Loopback endpoints have no listening server.
use serde_json::{json, Value};
use std::process::Command;

#[test]
fn removed_tool_spelling_is_refused_by_stock_provider_loaders() {
    let codex: Value =
        serde_json::from_str(include_str!("fixtures/profiles/gpt56solhigh.json")).unwrap();
    let cases = [
        (codex, "unsupported Codex setting 'disabled-tools'"),
        (
            json!({"provider": "openai", "model": "fixture-model"}),
            "unsupported profile setting(s): disabled-tools",
        ),
        (
            json!({"provider": "openai", "model": "fixture-model", "modelParams": {"chat_template_kwargs": {"enable_thinking": true}}}),
            "unsupported profile setting(s): disabled-tools",
        ),
        (
            json!({"provider": "anthropic", "model": "fixture-model"}),
            "unsupported Anthropic Messages setting(s): disabled-tools",
        ),
        (
            json!({"provider": "openai-responses", "model": "fixture-model"}),
            "unsupported OpenAI Responses setting",
        ),
        (
            json!({"provider": "openai", "model": "fixture-model", "ephemeralSettings": {"apiMode": "responses"}}),
            "unsupported OpenAI Responses setting",
        ),
    ];
    let root = tempfile::tempdir().unwrap();
    let profiles = root.path().join("profiles");
    std::fs::create_dir(&profiles).unwrap();
    for (mut profile, expected) in cases {
        if profile["provider"] != "codex" {
            if !profile["ephemeralSettings"].is_object() {
                profile["ephemeralSettings"] = json!({});
            }
            profile["ephemeralSettings"]["base-url"] = json!("http://127.0.0.1:1/v1");
            profile["ephemeralSettings"]["auth-key"] = json!("fixture-only");
        }
        for value in [json!([]), json!(["read_file"]), json!(false)] {
            profile["ephemeralSettings"]["disabled-tools"] = value;
            std::fs::write(profiles.join("unknown.json"), profile.to_string()).unwrap();
            let output = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
                .env("LLXPRT_CONFIG_HOME", root.path())
                .env_remove("LLXPRT_CONFIG_DIR")
                .env_remove("LLXPRT_BASE_URL")
                .env_remove("LLXPRT_MODEL_PARAMS_MODE")
                .env_remove("LLXPRT_REQUEST_TIMEOUT")
                .env_remove("LLXPRT_TURN_TIME")
                .env_remove("LLXPRT_MAX_TOOL_CALLS")
                .args([
                    "--profile",
                    "unknown",
                    "--session",
                    "removed-tool-spelling",
                    "--cwd",
                ])
                .arg(root.path())
                .args(["-p", "refuse before provider request"])
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(3), "{output:?}");
            let terminal: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(terminal["status"], "error", "{terminal}");
            let message = terminal["error"]["message"].as_str().unwrap();
            let expected_message = if profile["provider"] == "codex"
                || profile["provider"] == "openai-responses"
                || profile["ephemeralSettings"]["apiMode"] == "responses"
            {
                assert_eq!(terminal["error"]["code"], "profile-load");
                format!(
                    "profile resolution failed: profile {:?}: {expected}",
                    profiles.join("unknown.json").to_str().unwrap()
                )
            } else {
                assert_eq!(terminal["error"]["code"], "model-config");
                expected.to_string()
            };
            assert_eq!(message, expected_message);
        }
    }
}
