//! Compiled-CLI coverage for host-owned task settings. The command exits through
//! `--print-config`, after named-profile loading and before backend construction,
//! so it never reads real credentials or contacts a provider.

use serde_json::Value;
use std::process::Command;

const SCRUBBED_ENV: &[&str] = &[
    "LLXPRT_CONFIG_HOME",
    "LLXPRT_CONFIG_DIR",
    "XDG_CONFIG_HOME",
    "LLXPRT_BASE_URL",
    "LLXPRT_REQUEST_TIMEOUT",
    "LLXPRT_TURN_TIME",
    "LLXPRT_MAX_SHELL_OUTPUT",
    "LLXPRT_MAX_TOOL_OUTPUT",
    "LLXPRT_MAX_TURN_OUTPUT",
    // These env layers outrank the profile, so leaving them set in the ambient
    // environment would leak through `--print-config`.
    "LLXPRT_MAX_TOOL_CALLS",
    "LLXPRT_MODEL_PARAMS_MODE",
];

fn bin() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"));
    for key in SCRUBBED_ENV {
        command.env_remove(key);
    }
    command
}

fn printed_config(config_home: &std::path::Path, profile: Option<&str>) -> Value {
    let mut command = bin();
    command
        .env("LLXPRT_CONFIG_HOME", config_home)
        .arg("--turn-time")
        .arg("11s")
        .arg("--request-timeout")
        .arg("13s")
        .arg("--max-shell-output")
        .arg("17000")
        .arg("--max-tool-output")
        .arg("19000")
        .arg("--max-turn-output")
        .arg("23000")
        .arg("--print-config");
    if let Some(profile) = profile {
        command.arg("--profile").arg(profile);
    }
    let output = command.output().unwrap();

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn named_astra_shape_reaches_config_print_without_reassigning_deadlines() {
    let config = tempfile::tempdir().unwrap();
    let profiles = config.path().join("profiles");
    std::fs::create_dir(&profiles).unwrap();
    std::fs::write(
        profiles.join("astra-headless.json"),
        include_str!("fixtures/task-profile/astra-headless.json"),
    )
    .unwrap();

    std::fs::write(
        profiles.join("dsflash-mi300x.json"),
        r#"{
            "version": 1,
            "provider": "openai",
            "model": "paired-control-model",
            "ephemeralSettings": {
                "base-url": "https://paired-control.invalid/v1"
            }
        }"#,
    )
    .unwrap();

    let settings = printed_config(config.path(), Some("astra-headless"));
    let without_profile = printed_config(config.path(), None);
    let fixture_base_url = serde_json::json!({
        "value": "https://chatgpt.com/backend-api/codex",
        "source": "profile"
    });
    assert_eq!(settings["provider"]["base_url"], fixture_base_url);
    assert_ne!(without_profile["provider"]["base_url"], fixture_base_url);
    assert_eq!(
        settings["budgets"]["turn_time"],
        serde_json::json!({"value": 11, "source": "cli"})
    );
    assert_eq!(
        settings["budgets"]["request_timeout"],
        serde_json::json!({"value": 13, "source": "cli"})
    );
    assert_eq!(
        settings["budgets"]["max_shell_output"],
        serde_json::json!({"value": 17000, "source": "cli"})
    );
    assert_eq!(
        settings["budgets"]["max_tool_output"],
        serde_json::json!({"value": 19000, "source": "cli"})
    );
    assert_eq!(
        settings["budgets"]["max_turn_output"],
        serde_json::json!({"value": 23000, "source": "cli"})
    );
    assert!(!settings.to_string().contains("task-default-timeout"));
    assert!(!settings.to_string().contains("task-max-timeout"));
}
