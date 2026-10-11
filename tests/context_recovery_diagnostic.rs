//! Stock CLI terminal diagnostic for an unchanged oversized materialized request.

use serde_json::{json, Value};
use std::net::TcpListener;
use std::process::Command;

#[test]
fn unchanged_oversized_cli_request_explains_retained_mass_without_sending() {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let profile = root.path().join("fixture.json");
    std::fs::write(
        &profile,
        json!({"provider":"openai","model":"fixture","ephemeralSettings":{
            "base-url":format!("http://{}/v1", listener.local_addr().unwrap()),
            "auth-key":"public-diagnostic-fixture", "context-limit":10000,
        }})
        .to_string(),
    )
    .unwrap();
    let prompt = "界".repeat(25_000);
    let output = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
        .env("LLXPRT_CONFIG_HOME", root.path())
        .args(["--profile-load"])
        .arg(profile)
        .args(["--session", "diagnostic-cli", "--cwd"])
        .arg(root.path())
        .args(["--turn-time", "2s", "-p"])
        .arg(&prompt)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(5));
    let terminal: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(terminal["status"], "error");
    assert_eq!(terminal["error"]["code"], "context-limit");
    let message = terminal["error"]["message"].as_str().unwrap();
    let total: usize = message
        .strip_prefix("estimated request would be ")
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(message.contains(&format!(
        "({} bytes over the 30000-byte heuristic guard)",
        total - 30_000
    )));
    assert!(message.contains(&format!(
        "still {total} bytes ({} bytes over guard)",
        total - 30_000
    )));
    assert!(message.contains("reduced by 0 bytes"));
    assert!(message.contains("no eligible history or observed tool results could be removed"));
    assert!(message.contains("retained estimated bytes: system/instructions="));
    assert!(message.contains("prompts/notices="));
    assert!(message.contains("tool schemas="));
    assert!(message.contains("no request sent"));
    assert!(!message.contains(&prompt));
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}
