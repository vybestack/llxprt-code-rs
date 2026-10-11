//! Issue 251 exercises the stock CLI, provider mapping and persisted terminal boundary.
use super::*;

const PLACEHOLDER: &str = "I'll start by verifying the checkout state and understanding the project.\n\n<tool_called_placeholder>\n</tool_called_placeholder>";

fn assert_failed(envelope: &Value, root: &std::path::Path) {
    assert_eq!(envelope["status"], "error");
    assert_eq!(envelope["error"]["code"], "malformed_tool_call");
    assert_eq!(envelope["error"]["terminal_outcome"], "malformed_tool_call");
    assert!(envelope.get("summary").is_none());
    let message = envelope["error"]["message"].as_str().unwrap();
    assert!(message.contains("parsed to zero tool calls"));
    assert!(!message.contains("tool_called_placeholder"));
    assert!(!message.contains("verifying the checkout"));
    let store = store(root);
    let snapshot = store.snapshot().unwrap();
    assert_eq!(snapshot.branches[0].lifecycle, Lifecycle::Failed);
    assert!(!store.session_dir().join("tool-in-flight.json").exists());
}

#[test]
fn reported_zero_native_call_placeholder_fails_instead_of_wrapping_up() {
    for flags in [
        vec!["--max-tool-calls", "64", "--allow-shell"],
        vec!["--max-tool-calls", "64"],
    ] {
        let (envelope, requests, root) =
            run_with_flags(vec![completion(PLACEHOLDER, vec![])], 5, &flags);
        assert_failed(&envelope, root.path());
        assert_eq!(requests.len(), 1, "no fabricated call or provider reissue");
        let store = store(root.path());
        let snapshot = store.snapshot().unwrap();
        // No tool round was executed or manufactured from the placeholder.
        assert!(snapshot.branches[0].rounds.is_empty());
        assert!(
            store
                .start_request(
                    Some(1),
                    None,
                    "Inspect the workspace, then summarize.",
                    root.path(),
                )
                .unwrap()
                .retry
        );
    }
}

#[test]
fn forced_summary_placeholder_also_fails() {
    let (envelope, requests, root) = run(
        vec![completion("", vec![]), completion(PLACEHOLDER, vec![])],
        5,
    );
    assert_failed(&envelope, root.path());
    assert_eq!(requests.len(), 2);
    assert!(store(root.path()).snapshot().unwrap().branches[0]
        .rounds
        .iter()
        .all(|round| round.calls.is_empty()));
}

#[test]
fn placeholder_after_real_work_preserves_calls_but_cannot_claim_completion() {
    let (envelope, requests, root) = run(
        vec![
            completion(
                "Inspecting.",
                vec![tool("read", "list_directory", r#"{"path":"."}"#)],
            ),
            completion(PLACEHOLDER, vec![]),
        ],
        5,
    );
    assert_failed(&envelope, root.path());
    assert_eq!(requests.len(), 2);
    let snapshot = store(root.path()).snapshot().unwrap();
    let calls = snapshot.branches[0]
        .rounds
        .iter()
        .flat_map(|round| &round.calls)
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "list_directory");
    assert!(calls[0].ok);
}

#[test]
fn untagged_placeholder_mention_and_honest_no_tool_prose_still_succeed() {
    for text in [
        "The tool_called_placeholder marker is not an actual native call.",
        "This is a read-only explanation; no tool execution was requested.",
    ] {
        let (envelope, requests, root) = run(vec![completion(text, vec![])], 0);
        assert_eq!(envelope["status"], "ok");
        assert_eq!(envelope["summary"], text);
        assert_eq!(envelope["tool_calls"], 0);
        assert_eq!(envelope["terminal_outcome"], "wrap_up");
        assert_eq!(requests.len(), 1);
        assert_eq!(
            store(root.path()).snapshot().unwrap().branches[0].lifecycle,
            Lifecycle::Completed
        );
    }
}
