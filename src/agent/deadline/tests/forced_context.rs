//! Forced summary recovery uses the original paused clock and cancellation owner.
use super::*;

#[test]
fn forced_context_recovery_never_restarts_the_turn_clock() {
    let read = |id: &str| ToolCall {
        id: id.into(),
        name: "read_file".into(),
        args_json: r#"{"path":"evidence.txt","max_output_bytes":16000}"#.into(),
    };
    let mut fixture = Fixture::new(
        vec![
            reply(1, "read", vec![read("r1")]),
            reply(1, "read", vec![read("r2")]),
            reply(1, "read", vec![read("r3")]),
            reply(1, "", vec![]),
            Reply {
                delay: Duration::from_secs(1),
                result: Err("context length exceeded".into()),
            },
            reply(6, "natural final summary", vec![]),
        ],
        Some(10),
    );
    std::fs::write(
        fixture._root.path().join("evidence.txt"),
        "evidence line\n".repeat(1000),
    )
    .unwrap();
    fixture.assert_timeout(6, 3);
    let state = fixture.store.snapshot().unwrap();
    assert!(state.branches[0]
        .rounds
        .iter()
        .flat_map(|r| &r.calls)
        .all(|c| c.result_live.is_empty()));
}
