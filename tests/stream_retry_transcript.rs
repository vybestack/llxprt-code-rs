//! Retry metadata is durable and visible through the stock transcript subcommand.
use llxprt_code_rs::session::{SessionId, SessionStore, StreamRetryRecord};
use std::process::Command;

#[test]
fn persisted_model_retry_is_readable_as_json_and_human_text_after_reopen() {
    let root = tempfile::tempdir().unwrap();
    let id = SessionId::parse("retry-evidence").unwrap();
    let store = SessionStore::load_at(&id, root.path()).unwrap();
    let reserved = store.start_request(None, None, "P", root.path()).unwrap();
    store
        .record_stream_retry(
            &reserved,
            StreamRetryRecord {
                round: 1,
                retry: 1,
                delay_ms: 250,
                events: 1597,
            },
        )
        .unwrap();
    store.fail(&reserved, "transport exhausted", &[]).unwrap();
    drop(store);
    let command = |json: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"));
        command
            .env_clear()
            .env("LLXPRT_CONFIG_HOME", root.path())
            .args(["transcript", "--session", "retry-evidence"]);
        if json {
            command.arg("--json");
        }
        command.output().unwrap()
    };
    let output = command(true);
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["turns"][0]["model_retries"],
        serde_json::json!([
            {"round":1,"retry":1,"delay_ms":250,"events":1597}
        ])
    );
    assert_eq!(value["turns"][0]["lifecycle"], "failed");
    assert!(value["turns"][0]["rounds"].as_array().unwrap().is_empty());
    let output = command(false);
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8(output.stdout).unwrap().contains(
        "Model retry 1 / round 1: SSE transport interrupted (1597 events); backoff 250ms"
    ));
}
