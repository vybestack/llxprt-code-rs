use super::{run, tool_specs};
use crate::tools::output_limits::{MAX_SHELL_OUTPUT_DEFAULT, MAX_TOOL_OUTPUT_DEFAULT};
use serde_json::json;

fn run_with_limits(
    root: &std::path::Path,
    name: &str,
    args: serde_json::Value,
    max_tool_output: usize,
    max_shell_output: usize,
) -> (bool, String) {
    use crate::tools::{self, ShellConfig, ToolConfig, WorkspaceCap};
    tools::execute_tool(
        root,
        name,
        args,
        &ToolConfig {
            ws: WorkspaceCap::open(root).unwrap(),
            max_output_bytes: max_tool_output,
            shell: ShellConfig {
                max_shell_output,
                max_shell_timeout: std::time::Duration::from_secs(60),
                allow_shell: true,
            },
        },
    )
}

#[test]
fn tool_output_defaults_preserve_the_existing_exact_bounds() {
    assert_eq!(MAX_SHELL_OUTPUT_DEFAULT, 32 * 1024);
    assert_eq!(MAX_TOOL_OUTPUT_DEFAULT, 16 * 1024 * 1024);
    assert_eq!(
        MAX_TOOL_OUTPUT_DEFAULT,
        crate::limits::MAX_TURN_OUTPUT_BYTES
    );
}

#[test]
fn read_and_search_honor_per_call_output_caps() {
    let d = tempfile::tempdir().unwrap();
    let contents = (0..1000)
        .map(|index| format!("needle-{index}\n"))
        .collect::<String>();
    std::fs::write(d.path().join("big.txt"), contents).unwrap();

    let (ok, body) = run(
        d.path(),
        "read_file",
        json!({"path": "big.txt", "max_output_bytes": 256}),
    );
    assert!(ok, "{body}");
    assert!(
        body.len() <= 256,
        "read output exceeded per-call cap: {}",
        body.len()
    );

    let (ok, body) = run(
        d.path(),
        "search_file_content",
        json!({"pattern": "needle", "max_output_bytes": 256}),
    );
    assert!(ok, "{body}");
    assert!(
        body.len() <= 256,
        "search output exceeded per-call cap: {}",
        body.len()
    );

    let (ok, body) = run(
        d.path(),
        "read_file",
        json!({"path": "missing", "max_output_bytes": 8}),
    );
    assert!(!ok);
    assert!(
        body.len() <= 8,
        "read error exceeded per-call cap: {}",
        body.len()
    );

    let (ok, body) = run(
        d.path(),
        "search_file_content",
        json!({"pattern": "(", "max_output_bytes": 8}),
    );
    assert!(!ok);
    assert!(
        body.len() <= 8,
        "search error exceeded per-call cap: {}",
        body.len()
    );

    for (name, args) in [
        (
            "read_file",
            json!({"path": "big.txt", "max_output_bytes": "8"}),
        ),
        (
            "search_file_content",
            json!({"pattern": "needle", "max_output_bytes": "8"}),
        ),
    ] {
        let (ok, body) = run(d.path(), name, args);
        assert!(!ok);
        assert!(
            body.contains("'max_output_bytes' must be an integer"),
            "{body}"
        );
    }
}

#[test]
fn read_and_search_publish_per_call_output_caps() {
    for name in ["read_file", "search_file_content"] {
        let spec = tool_specs(false)
            .into_iter()
            .find(|spec| spec.name == name)
            .unwrap();
        let publishes_cap = spec
            .properties
            .iter()
            .any(|(property, _, required)| property == "max_output_bytes" && !required);
        assert!(
            publishes_cap,
            "{name} omitted max_output_bytes from its model-visible schema"
        );
    }
}

#[test]
fn raised_configured_caps_lift_the_hard_bounds() {
    let d = tempfile::tempdir().unwrap();
    let contents = (0..8000)
        .map(|index| format!("needle-{index}\n"))
        .collect::<String>();
    std::fs::write(d.path().join("big.txt"), contents).unwrap();

    // Under the built-in 32 KiB shell cap this `cat` is truncated; identical args
    // under a raised cap must publish more than the default ceiling.
    let command = json!({"command": "cat big.txt"});
    let (ok, body) = run_with_limits(
        d.path(),
        "run_shell_command",
        command.clone(),
        MAX_TOOL_OUTPUT_DEFAULT,
        MAX_SHELL_OUTPUT_DEFAULT,
    );
    assert!(ok, "{body}");
    assert!(
        body.len() <= 32 * 1024,
        "default shell cap did not truncate: {}",
        body.len()
    );

    let (ok, body) = run_with_limits(
        d.path(),
        "run_shell_command",
        command,
        128 * 1024,
        64 * 1024,
    );
    assert!(ok, "{body}");
    assert!(
        body.len() > 32 * 1024,
        "raised shell cap still truncated at the 32 KiB default: {}",
        body.len()
    );

    // A smaller per-call cap still bounds the result under the raised ceiling.
    let (ok, body) = run(
        d.path(),
        "read_file",
        json!({"path": "big.txt", "max_output_bytes": 256}),
    );
    assert!(ok, "{body}");
    assert!(body.len() <= 256, "{}", body.len());
}
