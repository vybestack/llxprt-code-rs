//! #220: exercise the registered production editor boundary, not a simulated write.
use llxprt_code_rs::tools::{execute_tool, tool_specs, ShellConfig, ToolConfig, WorkspaceCap};
use serde_json::json;
use std::time::Duration;

#[test]
fn block_as_whole_file_write_succeeds_but_destroys_preservation() {
    let cwd = tempfile::tempdir().unwrap();
    let original = "use std::sync::Mutex;\n#[test]\nfn kept() {}\n#[test]\nfn target() { assert_eq!(1, 1); }\n";
    let block = "fn target() { assert_eq!(2, 2); }";
    std::fs::write(cwd.path().join("phase2.rs"), original).unwrap();
    let config = ToolConfig {
        ws: WorkspaceCap::open(cwd.path()).unwrap(),
        max_output_bytes: 1024 * 1024,
        digest_size_floor: llxprt_code_rs::context_ingress::filter::DEFAULT_DIGEST_SIZE_FLOOR,
        shell: ShellConfig {
            max_shell_output: 1024,
            max_shell_timeout: Duration::from_secs(1),
            allow_shell: false,
        },
    };
    assert!(tool_specs(false).iter().any(|s| s.name == "write_file"));
    assert!(tool_specs(false).iter().any(|s| s.name == "replace"));
    let (ok, _) = execute_tool(
        cwd.path(),
        "write_file",
        json!({"path": "phase2.rs", "content": block}),
        &config,
    );
    assert!(ok, "whole-file semantics are not themselves a tool failure");
    let damaged = std::fs::read_to_string(cwd.path().join("phase2.rs")).unwrap();
    assert_eq!(damaged, block);
    assert!(!damaged.contains("fn kept()"));

    // An explicit authorized restoration, not an automatic tool fallback.
    let (ok, _) = execute_tool(
        cwd.path(),
        "write_file",
        json!({"path": "phase2.rs", "content": original}),
        &config,
    );
    assert!(ok);
    let (ok, result) = execute_tool(
        cwd.path(),
        "replace",
        json!({"path": "phase2.rs", "old_string": "assert_eq!(1, 1)",
               "new_string": "assert_eq!(2, 2)"}),
        &config,
    );
    assert!(ok, "{result}");
    assert_eq!(
        std::fs::read_to_string(cwd.path().join("phase2.rs")).unwrap(),
        original.replace("assert_eq!(1, 1)", "assert_eq!(2, 2)")
    );
    let before = std::fs::read(cwd.path().join("phase2.rs")).unwrap();
    let (ok, _) = execute_tool(
        cwd.path(),
        "replace",
        json!({"path": "phase2.rs", "old_string": "assert_eq!(1, 1)",
               "new_string": "assert_eq!(3, 3)"}),
        &config,
    );
    assert!(!ok, "a stale block must not silently overwrite the suite");
    assert_eq!(std::fs::read(cwd.path().join("phase2.rs")).unwrap(), before);
}
