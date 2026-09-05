use llxprt_code_rs::agent::CompletedRun;
use llxprt_code_rs::cli::{self, AppError, Code, RunOutcome};
use llxprt_code_rs::session::SessionId;
use std::process::Command;

#[test]
fn success_envelope_bytes_are_pinned() {
    let outcome = Ok(RunOutcome {
        session: SessionId::parse("sess_1").unwrap(),
        session_dir: "/sessions/sess_1".into(),
        run: CompletedRun {
            turn: 2,
            attempt: 1,
            branch_id: "branch-\"snow-雪".into(),
            summary: "done\n雪".into(),
            tool_count: 3,
            declared_tool_calls: None,
            budget_exhausted: false,
            zero_call_tail: 2,
            prompt_digest: "0123456789abcdef".into(),
            status: "ok".into(),
            terminal_outcome: None,
            branch: false,
            replayed: true,
        },
    });
    let line = cli::envelope(&outcome, "sess_1").to_line();
    assert_eq!(
        String::from_utf8_lossy(&line),
        "{\"attempt\":1,\"branch\":false,\"branch_id\":\"branch-\\\"snow-雪\",\"budget_exhausted\":false,\"declared_tool_calls\":-1,\"prompt_digest\":\"0123456789abcdef\",\"replayed\":true,\"session_dir\":\"/sessions/sess_1\",\"session_id\":\"sess_1\",\"status\":\"ok\",\"summary\":\"done\\n雪\",\"tool_calls\":3,\"turn\":2,\"zero_call_tail\":2}\n"
    );
    assert_eq!(
        line,
        b"{\"attempt\":1,\"branch\":false,\"branch_id\":\"branch-\\\"snow-\xe9\x9b\xaa\",\"budget_exhausted\":false,\"declared_tool_calls\":-1,\"prompt_digest\":\"0123456789abcdef\",\"replayed\":true,\"session_dir\":\"/sessions/sess_1\",\"session_id\":\"sess_1\",\"status\":\"ok\",\"summary\":\"done\\n\xe9\x9b\xaa\",\"tool_calls\":3,\"turn\":2,\"zero_call_tail\":2}\n"
    );
}

#[test]
fn nested_error_envelope_bytes_are_pinned() {
    let outcome: Result<RunOutcome, AppError> =
        Err(AppError::new(Code::Model, "model-\"bad", "line one\n雪"));
    let line = cli::envelope(&outcome, "sess_1").to_line();
    assert_eq!(
        String::from_utf8_lossy(&line),
        "{\"error\":{\"code\":\"model-\\\"bad\",\"message\":\"line one\\n雪\"},\"session_id\":\"sess_1\",\"status\":\"error\"}\n"
    );
    assert_eq!(
        line,
        b"{\"error\":{\"code\":\"model-\\\"bad\",\"message\":\"line one\\n\xe9\x9b\xaa\"},\"session_id\":\"sess_1\",\"status\":\"error\"}\n"
    );
}

#[test]
fn clap_usage_envelope_bytes_are_pinned() {
    let output = Command::new(env!("CARGO_BIN_EXE_llxprt-code-rs"))
        .args(["--session", "sess_1", "--not-a-real-option"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "{\"error\":{\"code\":\"usage\",\"message\":\"invalid arguments\"},\"session_id\":\"sess_1\",\"status\":\"error\"}\n"
    );
    assert_eq!(
        output.stdout,
        b"{\"error\":{\"code\":\"usage\",\"message\":\"invalid arguments\"},\"session_id\":\"sess_1\",\"status\":\"error\"}\n"
    );
    assert!(output.stderr.is_empty());
}
