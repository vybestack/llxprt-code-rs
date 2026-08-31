use super::{run, tool_specs};
use crate::tools::output_limits::{MAX_SHELL_OUTPUT_DEFAULT, MAX_TOOL_OUTPUT_DEFAULT};
use serde_json::json;

#[test]
fn tool_output_defaults_preserve_the_existing_exact_bounds() {
    assert_eq!(MAX_SHELL_OUTPUT_DEFAULT, 32 * 1024);
    assert_eq!(MAX_TOOL_OUTPUT_DEFAULT, 16 * 1024 * 1024);
    assert_eq!(MAX_TOOL_OUTPUT_DEFAULT, crate::agent::MAX_TURN_OUTPUT_BYTES);
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
