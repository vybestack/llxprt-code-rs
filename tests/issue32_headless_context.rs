//! Issue #32: the original headless guard, progress survival and same-session restart.
//! Scripted loopback replies drive the stock CLI and real shell tools, not a live model.

#[path = "issue32_context/fixture.rs"]
mod fixture;

use fixture::{shell_reply, Fixture, GUARD};
use llxprt_code_rs::session::Lifecycle;
use serde_json::json;

#[test]
fn large_shell_transcripts_compact_and_final_report_survives() {
    let fixture = Fixture::new();
    let mut replies = (1..=3)
        .map(|i| shell_reply(&format!("test-{i}"), 280_000, false))
        .collect::<Vec<_>>();
    replies.push(json!({"role":"assistant","content":""}));
    replies.push(json!({"role":"assistant","content":"Tests completed; retained final report."}));
    // The server checks file-backed stderr BEFORE releasing the final response.
    // Capturing output only after process exit would not prove live progress.
    let server = fixture.serve(replies, Some(4));
    let (status, terminal, events) =
        fixture.run("large", "Run the bounded test transcripts.", true);
    assert_eq!(status, 0, "{terminal}");
    assert_eq!(
        terminal["summary"],
        "Tests completed; retained final report."
    );
    assert_eq!(terminal["tool_calls"], 3);
    assert_eq!(terminal["declared_tool_calls"], 512);
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 5);
    assert!(requests[4]
        .to_string()
        .contains("final plain-text summary now"));
    let before = requests[2].to_string().len();
    let after = requests[3].to_string().len();
    assert!(after < before - 250_000, "wire bytes: {before} -> {after}");
    let returns = fixture::tool_returns(&requests[3]);
    assert_eq!(returns.len(), 3);
    for (i, result) in returns.iter().enumerate() {
        assert_eq!(result["tool_call_id"], format!("test-{}", i + 1));
        let text = result["content"].as_str().unwrap();
        if i < 2 {
            assert!(text.contains("CTXDIGEST v1"));
            assert!(!text.contains(&"x".repeat(1000)));
        } else {
            assert!(text.contains(&"x".repeat(280_000)), "latest evidence lost");
        }
    }
    assert!(requests[3]
        .to_string()
        .contains("Run the bounded test transcripts."));
    let live: Vec<_> = events
        .iter()
        .filter(|e| e["type"] == "tool_result")
        .collect();
    assert_eq!(live.len(), 3);
    assert!(live
        .iter()
        .all(|e| e["result"].as_str().unwrap().contains("test result: ok")));
    let state = fixture.state();
    assert_eq!(state.branches[0].lifecycle, Lifecycle::Completed);
    assert_eq!(state.branches[0].rounds.len(), 4);
    fixture::assert_durable_calls(&state.branches[0].rounds, 3);
    println!("issue32 original guard={GUARD}; recovered wire bytes={before}->{after}");
}

#[test]
fn unavoidable_limit_keeps_progress_work_and_same_session_usable() {
    let fixture = Fixture::new();
    // A completed predecessor must survive a failed continuation, too.
    let server = fixture.serve(
        vec![json!({"role":"assistant","content":"Prior task completed."})],
        None,
    );
    let (status, _, events) = fixture.run("prior", "Establish the prior task.", false);
    assert_eq!(status, 0);
    assert!(events.is_empty(), "default stderr must remain silent");
    assert_eq!(server.join().unwrap().len(), 1);

    let first = shell_reply("large-a", 400_000, true);
    let second = shell_reply("large-b", 400_000, false);
    let mut batch = first;
    batch["tool_calls"]
        .as_array_mut()
        .unwrap()
        .push(second["tool_calls"][0].clone());
    let server = fixture.serve(vec![batch], None);
    let (status, terminal, events) = fixture.run("failed", "Repair and test the workspace.", true);
    assert_eq!(status, 5, "{terminal}");
    assert_eq!(terminal["status"], "error");
    assert_eq!(terminal["error"]["code"], "context-limit");
    assert!(
        terminal.get("summary").is_none(),
        "never fabricate a final report"
    );
    let message = terminal["error"]["message"].as_str().unwrap();
    assert!(message.contains("786432-byte heuristic guard"));
    assert!(message.contains("reduced by 0 bytes"));
    assert!(message.contains("latest tool evidence are retained"));
    assert!(message.contains("no request sent"));
    assert_eq!(
        server.join().unwrap().len(),
        1,
        "no oversized request or unchanged retry"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("work.txt")).unwrap(),
        "repaired\n"
    );
    assert_eq!(events[0]["type"], "assistant_text");
    assert_eq!(
        events[0]["text"],
        "Running bounded test transcripts; work is retained."
    );
    let results: Vec<_> = events
        .iter()
        .filter(|e| e["type"] == "tool_result")
        .collect();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["id"], "large-a");
    assert_eq!(results[1]["id"], "large-b");
    assert!(results
        .iter()
        .all(|e| e["ok"] == true && e["result"].as_str().unwrap().contains("test result: ok")));
    let failed = fixture.state().branches[1].clone();
    assert_eq!(failed.lifecycle, Lifecycle::Failed);
    assert_eq!(failed.summary, "");
    assert_eq!(failed.error, message);
    fixture::assert_durable_calls(&failed.rounds, 2);

    let transcript = fixture.lookback();
    assert_eq!(transcript["turns"][1]["lifecycle"], "failed");
    assert_eq!(
        transcript["turns"][1]["rounds"][0]["text"],
        events[0]["text"]
    );
    assert_eq!(
        transcript["turns"][1]["rounds"][0]["calls"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // A fresh process uses the SAME profile, limit, workspace and session id.
    let server = fixture.serve(
        vec![json!({"role":"assistant","content":"Follow-up report retained."})],
        None,
    );
    let (status, terminal, events) =
        fixture.run("followup", "Continue from the retained workspace.", false);
    assert_eq!(status, 0, "{terminal}");
    assert_eq!(
        terminal["session_id"],
        "task-ae84342303192f1edb23fae74512cf66"
    );
    assert_eq!(terminal["turn"], 2);
    assert_eq!(terminal["attempt"], 2);
    assert_eq!(terminal["replayed"], false);
    assert_eq!(terminal["summary"], "Follow-up report retained.");
    assert!(events.is_empty());
    let requests = server.join().unwrap();
    let wire = requests[0].to_string();
    assert!(wire.contains("Prior task completed."));
    assert!(wire.contains("Continue from the retained workspace."));
    assert!(!wire.contains("large-a"));
    assert!(!wire.contains("Repair and test the workspace."));
    let state = fixture.state();
    assert_eq!(state.branches.len(), 3);
    assert_eq!(
        serde_json::to_value(&state.branches[1]).unwrap(),
        serde_json::to_value(&failed).unwrap(),
        "failed evidence must not be overwritten"
    );
    assert_eq!(state.branches[2].lifecycle, Lifecycle::Completed);
    assert_eq!(
        state.branches[2].parent_branch,
        Some(state.branches[0].branch_id.clone())
    );
}
