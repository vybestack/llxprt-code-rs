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
];

fn bin() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"));
    for key in SCRUBBED_ENV {
        command.env_remove(key);
    }
    command
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

    let output = bin()
        .env("LLXPRT_CONFIG_HOME", config.path())
        .arg("--profile")
        .arg("astra-headless")
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
        .arg("--print-config")
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let settings: Value = serde_json::from_slice(&output.stdout).unwrap();
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
