//! Composition assertions: cache files stay separate from selected live transcript.
use serde_json::Value;
use std::path::Path;
use std::process::Command;

pub(super) fn assert_streams(root: &Path, turn: &str, stderr: &[u8], envelope: &Value) {
    let cache = std::fs::read_to_string(root.join(format!("cache-{turn}.jsonl"))).unwrap();
    let observations: Vec<Value> = cache
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let calls: Vec<_> = observations
        .iter()
        .filter(|e| e["event"] == "prompt_cache_call")
        .collect();
    assert_eq!(calls.len(), if turn == "1" { 6 } else { 2 });
    for (index, call) in calls.iter().enumerate() {
        assert_eq!(call["reported_input_tokens"], 100);
        assert_eq!(call["cached_input_tokens"], 20);
        assert_eq!(call["call"], index + 1);
    }
    assert_eq!(observations.len(), calls.len() * 2);
    assert_eq!(observations.last().unwrap()["usage"]["calls"], calls.len());
    let live: Vec<Value> = std::str::from_utf8(stderr)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(live
        .iter()
        .all(|e| e["type"] == "assistant_text" || e["type"] == "tool_result"));
    let last = live.last().unwrap();
    assert_eq!(last["type"], "assistant_text");
    assert_eq!(last["text"], envelope["summary"]);
    assert_eq!(
        live.iter().filter(|e| e["type"] == "tool_result").count(),
        if turn == "1" { 5 } else { 1 }
    );
    if turn == "1" {
        let read = live.iter().find(|e| e["type"] == "tool_result").unwrap();
        assert!(read["result"]
            .as_str()
            .unwrap()
            .contains("bounded public evidence"));
    }
    assert!(!cache.contains("assistant_text"));
    assert!(!std::str::from_utf8(stderr)
        .unwrap()
        .contains("prompt_cache_call"));
}

pub(super) fn assert_restored_transcript(root: &Path) {
    let output = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
        .env("LLXPRT_CONFIG_HOME", root)
        .args(["transcript", "--session", "effective-native", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let transcript: Value = serde_json::from_slice(&output.stdout).unwrap();
    let turns = transcript["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 2);
    assert_eq!(
        turns[0]["summary"],
        "Fixed addition; the native tool test passed."
    );
    assert!(turns[0]["rounds"][0]["calls"][0]["result"]
        .as_str()
        .unwrap()
        .contains("CTXDIGEST v1"));
    assert_eq!(
        turns[1]["summary"],
        "Restored session verified the addition implementation."
    );
}
