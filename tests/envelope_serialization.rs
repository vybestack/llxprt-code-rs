use llxprt_code_rs::agent::CompletedRun;
use llxprt_code_rs::cli::{self, AppError, Code, RunOutcome};
use llxprt_code_rs::session::SessionId;
use std::process::Command;

#[test]
fn success_envelope_bytes_are_pinned() {
    let outcome = Ok(RunOutcome {
        session: SessionId::parse("sess_1").unwrap(),
        session_dir: "/sessions/sess_1".into(),
        output_caps: llxprt_code_rs::agent::OutputCaps {
            shell: 32768,
            tool: 16777216,
            turn: 16777216,
        },
        run: CompletedRun {
            request_attempts: Default::default(),
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
        "{\"attempt\":1,\"branch\":false,\"branch_id\":\"branch-\\\"snow-雪\",\"budget_exhausted\":false,\"declared_tool_calls\":-1,\"output_caps\":{\"shell\":32768,\"tool\":16777216,\"turn\":16777216},\"prompt_digest\":\"0123456789abcdef\",\"replayed\":true,\"request_attempts\":{\"attempts\":0,\"retries\":0},\"session_dir\":\"/sessions/sess_1\",\"session_id\":\"sess_1\",\"status\":\"ok\",\"summary\":\"done\\n雪\",\"tool_calls\":3,\"turn\":2,\"zero_call_tail\":2}\n"
    );
    assert_eq!(
        line,
        b"{\"attempt\":1,\"branch\":false,\"branch_id\":\"branch-\\\"snow-\xe9\x9b\xaa\",\"budget_exhausted\":false,\"declared_tool_calls\":-1,\"output_caps\":{\"shell\":32768,\"tool\":16777216,\"turn\":16777216},\"prompt_digest\":\"0123456789abcdef\",\"replayed\":true,\"request_attempts\":{\"attempts\":0,\"retries\":0},\"session_dir\":\"/sessions/sess_1\",\"session_id\":\"sess_1\",\"status\":\"ok\",\"summary\":\"done\\n\xe9\x9b\xaa\",\"tool_calls\":3,\"turn\":2,\"zero_call_tail\":2}\n"
    );
}

#[test]
fn nested_error_envelope_bytes_are_pinned() {
    let outcome: Result<RunOutcome, AppError> =
        Err(AppError::new(Code::Model, "model-\"bad", "line one\n雪"));
    let line = cli::envelope(&outcome, "sess_1").to_line();
    assert_eq!(
        String::from_utf8_lossy(&line),
        "{\"error\":{\"code\":\"model-\\\"bad\",\"message\":\"line one\\n雪\"},\"request_attempts\":{\"attempts\":0,\"retries\":0},\"session_id\":\"sess_1\",\"status\":\"error\"}\n"
    );
    assert_eq!(
        line,
        b"{\"error\":{\"code\":\"model-\\\"bad\",\"message\":\"line one\\n\xe9\x9b\xaa\"},\"request_attempts\":{\"attempts\":0,\"retries\":0},\"session_id\":\"sess_1\",\"status\":\"error\"}\n"
    );
}

/// An error the run decorated with its own terminal outcome carries that verdict into the
/// nested error detail (issue 146 malformed tool call, issue 153 exhausted truncation
/// retry), so a supervisor can branch without parsing the message.
#[test]
fn declared_terminal_outcome_rides_the_error_envelope() {
    for outcome in [
        llxprt_code_rs::agent::MALFORMED_TOOL_CALL_KEY,
        llxprt_code_rs::agent::TRUNCATED_OUTPUT_RETRIED_KEY,
    ] {
        let mut error = AppError::new(Code::Model, "finish-reason", "truncated");
        error.terminal_outcome = Some(outcome);
        let line = cli::envelope(&Err(error), "sess_1").to_line();
        let value: serde_json::Value = serde_json::from_slice(&line).unwrap();
        assert_eq!(value["status"], "error");
        assert_eq!(value["error"]["code"], "finish-reason");
        assert_eq!(value["error"]["terminal_outcome"], outcome);
    }
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
        "{\"error\":{\"code\":\"usage\",\"message\":\"invalid arguments\"},\"request_attempts\":{\"attempts\":0,\"retries\":0},\"session_id\":\"sess_1\",\"status\":\"error\"}\n"
    );
    assert_eq!(
        output.stdout,
        b"{\"error\":{\"code\":\"usage\",\"message\":\"invalid arguments\"},\"request_attempts\":{\"attempts\":0,\"retries\":0},\"session_id\":\"sess_1\",\"status\":\"error\"}\n"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn terminal_and_profile_errors_retain_started_retry_counts() {
    for code in [Code::Model, Code::Session, Code::Profiling] {
        let mut error = AppError::new(code, "model", "failed");
        error.request_attempts = Box::new(llxprt_code_rs::envelope::RequestAttempts {
            attempts: 3,
            retries: 1,
        });
        let document = cli::envelope(&Err(error), "sess").to_value();
        assert_eq!(
            document["request_attempts"],
            serde_json::json!({"attempts":3,"retries":1})
        );
    }
}
