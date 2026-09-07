use super::*;
use serde_json::json;
use std::time::Duration;

/// An exact read limit of 0 bytes returns no content and is never a panic, and a
/// limit larger than the file returns the whole file with no truncation marker.
#[test]
fn shell_timeout_defaults_clamps_and_reports_effective_value() {
    let d = tempfile::tempdir().unwrap();
    let config = ToolConfig {
        ws: WorkspaceCap::open(d.path()).unwrap(),
        max_output_bytes: 16 * 1024,
        shell: ShellConfig {
            default_shell_timeout: Duration::from_secs(2),
            max_shell_output: 64 * 1024,
            max_shell_timeout: Duration::from_secs(3),
            allow_shell: true,
        },
    };
    // Omitted timeout uses the configured default and ordinary success text stays unchanged.
    let (ok, output) = execute_tool(
        d.path(),
        "run_shell_command",
        json!({"command": "printf stable"}),
        &config,
    );
    assert!(ok, "{output}");
    assert_eq!(output, "stable");
    // A positive explicit value below the maximum preserves ordinary success text.
    let (ok, output) = execute_tool(
        d.path(),
        "run_shell_command",
        json!({"command": "printf explicit", "timeout_seconds": 2}),
        &config,
    );
    assert!(ok, "{output}");
    assert_eq!(output, "explicit");
    let (ok, output) = execute_tool(
        d.path(),
        "run_shell_command",
        json!({"command": "printf clamped", "timeout_seconds": 99}),
        &config,
    );
    assert!(ok, "{output}");
    assert!(
        output.contains("requested timeout 99s; effective timeout 3s"),
        "{output}"
    );
    // The shared runner's process-group cleanup remains observable through the timeout path.
    let escaped = d.path().join("must-not-survive");
    let command = format!("(sleep 2; touch {}) & wait", escaped.display());
    let (ok, output) = execute_tool(
        d.path(),
        "run_shell_command",
        json!({"command": command, "timeout_seconds": 1}),
        &config,
    );
    assert!(!ok);
    assert!(output.contains("timed out after 1000 ms"), "{output}");
    std::thread::sleep(Duration::from_millis(1200));
    assert!(
        !escaped.exists(),
        "timeout cleanup must kill the whole shell process group"
    );
}

#[test]
fn shell_nonzero_and_signal_return_ok_false() {
    let d = tempfile::tempdir().unwrap();
    let ws = WorkspaceCap::open(d.path()).unwrap();
    let c = ToolConfig {
        ws,
        max_output_bytes: 16 * 1024,
        shell: ShellConfig {
            default_shell_timeout: Duration::from_secs(60),
            max_shell_output: 64 * 1024,
            max_shell_timeout: Duration::from_secs(30),
            allow_shell: true,
        },
    };
    let args = json!({"command": "exit 3"});
    let (ok, msg) = execute_tool(d.path(), "run_shell_command", args, &c);
    assert!(!ok, "nonzero exit must be ok=false: {msg}");
    let (ok, _) = execute_tool(
        d.path(),
        "run_shell_command",
        json!({"command": "true"}),
        &c,
    );
    assert!(ok);
    // Without --allow-shell the tool refuses even a valid command.
    let c2 = ToolConfig {
        shell: ShellConfig {
            allow_shell: false,
            ..c.shell
        },
        ..c
    };
    let (ok, _) = execute_tool(
        d.path(),
        "run_shell_command",
        json!({"command": "true"}),
        &c2,
    );
    assert!(!ok);
}

/// `-1` is the explicit unlimited request, which remains bounded by the configured maximum.
#[test]
fn shell_unlimited_timeout_uses_the_maximum_and_rejects_other_nonpositive_values() {
    let d = tempfile::tempdir().unwrap();
    let config = ToolConfig {
        ws: WorkspaceCap::open(d.path()).unwrap(),
        max_output_bytes: 16 * 1024,
        shell: ShellConfig {
            default_shell_timeout: Duration::from_secs(1),
            max_shell_output: 64 * 1024,
            max_shell_timeout: Duration::from_secs(2),
            allow_shell: true,
        },
    };
    let (ok, output) = execute_tool(
        d.path(),
        "run_shell_command",
        json!({"command": "sleep 3", "timeout_seconds": -1}),
        &config,
    );
    assert!(
        !ok,
        "unlimited must still use the configured finite maximum: {output}"
    );
    assert!(output.contains("timed out after 2000 ms"), "{output}");
    assert!(
        output.contains("requested timeout unlimited; effective timeout 2s"),
        "{output}"
    );

    let marker = d.path().join("must-not-spawn");
    let command = format!("touch {}", marker.display());
    for timeout_seconds in [json!(0), json!(-2)] {
        let (ok, output) = execute_tool(
            d.path(),
            "run_shell_command",
            json!({"command": command, "timeout_seconds": timeout_seconds}),
            &config,
        );
        assert!(!ok, "nonpositive timeout must be rejected: {output}");
        assert!(
            output.contains("must be -1 (unlimited) or a positive integer"),
            "{output}"
        );
        assert!(!marker.exists(), "invalid timeout must reject before spawn");
    }
}
