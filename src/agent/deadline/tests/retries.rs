use super::*;
use crate::adapter::ModelFailure;
use crate::envelope::RequestAttempts;
use serdes_ai::models::error::{TransportDetail, TransportOrigin};
use serdes_ai::models::ModelError;

fn failure(error: ModelError) -> Reply {
    Reply {
        delay: Duration::ZERO,
        result: Err(ModelFailure::from_model_error(error)),
    }
}

fn status(code: u16, hint: Option<u64>) -> Reply {
    failure(ModelError::Transport(TransportDetail::from_status(
        code,
        None,
        0,
        hint.map(Duration::from_secs),
    )))
}

fn counts(attempts: u64, retries: u64) -> RequestAttempts {
    RequestAttempts { attempts, retries }
}

#[test]
fn mapped_throttle_server_and_connectivity_retry_to_success_without_tools() {
    for first in [
        status(429, Some(2)),
        status(503, None),
        failure(ModelError::Timeout),
        failure(ModelError::Connection("secret endpoint".into())),
    ] {
        let mut f = Fixture::new(vec![first, reply(0, "OK", vec![])], Some(20));
        let run = f.run().unwrap();
        assert_eq!(run.request_attempts, counts(2, 1));
        assert_eq!(run.tool_count, 0);
        assert_eq!(f.agent.model_calls(), 2);
        assert_eq!(f.store.snapshot().unwrap().branches[0].rounds.len(), 1);
    }
}

#[test]
fn retry_after_waits_at_least_hint_and_never_restarts_deadline() {
    let mut f = Fixture::new(vec![status(429, Some(7)), reply(4, "OK", vec![])], Some(10));
    f.assert_timeout(2, 0);
    // There is no second attempt if the deadline falls during backoff.
    let mut f = Fixture::new(vec![status(429, Some(15))], Some(10));
    let error = f.run().unwrap_err();
    assert_eq!(error.key, "turn-time-exhausted");
    assert_eq!(error.request_attempts, counts(1, 0));
    assert_eq!(f.active.get(), 0);
    // An enormous hint cannot create an unbounded sleep, even with no deadline.
    for budget in [None, Some(100)] {
        let mut f = Fixture::new(vec![status(429, Some(u64::MAX))], budget);
        let error = f.run().unwrap_err();
        assert_eq!(error.envelope_code, Some("model-rate-limit"));
        assert_eq!(error.request_attempts, counts(1, 0));
    }
    // A nearer deadline is still the controlling exit, including huge hints.
    let mut f = Fixture::new(vec![status(429, Some(u64::MAX))], Some(10));
    f.assert_timeout(1, 0);
}

#[test]
fn exhausted_retry_budget_surfaces_last_typed_failure_and_exact_counts() {
    let mut f = Fixture::new((0..4).map(|_| status(503, None)).collect(), None);
    let error = f.run().unwrap_err();
    assert_eq!(error.envelope_code, Some("model-transient-server"));
    assert_eq!(error.request_attempts, counts(4, 3));
    assert_eq!(f.agent.model_calls(), 4);
    assert_eq!(f.store.snapshot().unwrap().branches[0].rounds.len(), 0);
}

#[test]
fn mapped_auth_validation_protocol_and_cancellation_remain_terminal() {
    let decode = ModelError::Transport(TransportDetail {
        origin: TransportOrigin::Decode,
        url_class: None,
        body_prefix: None,
        body_bytes: 0,
        retry_after: None,
    });
    for first in [
        status(401, None),
        status(400, None),
        failure(ModelError::Authentication("secret".into())),
        failure(ModelError::Configuration("invalid params".into())),
        failure(ModelError::InvalidResponse("rate limited; retry me".into())),
        failure(ModelError::Cancelled),
        failure(decode),
    ] {
        let mut f = Fixture::new(vec![first], Some(10));
        let error = f.run().unwrap_err();
        assert_eq!(error.request_attempts, counts(1, 0));
        assert_eq!(f.agent.model_calls(), 1);
    }
}

#[test]
fn subsequent_request_retry_never_dispatches_a_tool_twice() {
    let write = ToolCall {
        id: "write-once".into(),
        name: "write_file".into(),
        args_json: r#"{"path":"once","content":"done"}"#.into(),
    };
    let mut f = Fixture::new(
        vec![
            reply(0, "", vec![write]),
            status(429, Some(1)),
            reply(0, "OK", vec![]),
        ],
        Some(20),
    );
    let run = f.run().unwrap();
    assert_eq!(run.request_attempts, counts(3, 1));
    assert_eq!(run.tool_count, 1);
    let snapshot = f.store.snapshot().unwrap();
    assert_eq!(
        snapshot.branches[0]
            .rounds
            .iter()
            .map(|r| r.calls.len())
            .sum::<usize>(),
        1
    );
    assert_eq!(
        std::fs::read_to_string(f._root.path().join("once")).unwrap(),
        "done"
    );
}

#[test]
fn retry_then_malformed_reply_is_terminal_without_dispatch() {
    let mut malformed = list();
    malformed.args_json = "not JSON".into();
    let mut f = Fixture::new(
        vec![status(503, None), reply(0, "", vec![malformed])],
        Some(20),
    );
    let error = f.run().unwrap_err();
    assert_eq!(error.request_attempts, counts(2, 1));
    assert_eq!(f.agent.model_calls(), 2);
    assert!(f.store.snapshot().unwrap().branches[0]
        .rounds
        .iter()
        .all(|r| r.calls.is_empty()));
}

#[test]
fn deadline_during_initial_attempt_reports_one_attempt_no_retry() {
    let mut f = Fixture::new(vec![reply(20, "OK", vec![])], Some(10));
    let error = f.run().unwrap_err();
    assert_eq!(error.request_attempts, counts(1, 0));
    assert_eq!(f.active.get(), 0);
}

#[test]
fn mapped_exhaustion_scrubs_secrets_and_bounds_diagnostics() {
    let mut f = Fixture::new(
        (0..4)
            .map(|_| {
                failure(ModelError::Transport(TransportDetail::from_status(
                    503,
                    Some(format!("secret-marker {}", "x".repeat(2000))),
                    2014,
                    None,
                )))
            })
            .collect(),
        None,
    );
    f.agent = f.agent.with_secrets(vec!["secret-marker".into()]);
    let error = f.run().unwrap_err();
    assert!(!error.message.contains("secret-marker"));
    assert!(error.message.len() <= crate::redact::MAX_DIAGNOSTIC_BYTES);
    assert_eq!(error.request_attempts, counts(4, 3));
}

#[test]
fn request_retry_preserves_absent_usage_and_reported_zero() {
    for usage in [
        LlmUsage::default(),
        LlmUsage {
            request_tokens: Some(0),
            ..Default::default()
        },
    ] {
        let mut success = reply(0, "OK", vec![]);
        success.result.as_mut().unwrap().usage = usage.clone();
        let f = Fixture::new(vec![status(429, Some(1)), success], Some(20));
        let turn = Turn::new(&f.agent, &f.store, &f.reserved).unwrap();
        turn.runtime.block_on(async { tokio::time::pause() });
        let _entered = turn.runtime.enter();
        let result = turn
            .round(&[], &[])
            .unwrap_or_else(|_| panic!("retry failed"));
        assert_eq!(result.usage, usage);
        assert_eq!(turn.request_attempts.get(), counts(2, 1));
    }
}
