use super::*;
use crate::adapter::{LlmUsage, ModelFuture};
use crate::session::{Lifecycle, SessionId};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

struct Script {
    replies: RefCell<VecDeque<Result<LlmResult, crate::adapter::BackendError>>>,
    inputs: Rc<RefCell<Vec<String>>>,
    interruption: Cell<Option<usize>>,
}
impl ChatBackend for Script {
    fn stream_interruption(&self) -> Option<usize> {
        self.interruption.get()
    }
    fn request<'a>(
        &'a self,
        requests: &'a [serdes_ai::core::ModelRequest],
        _: &'a [crate::tools::ToolSpec],
    ) -> ModelFuture<'a> {
        Box::pin(async move {
            self.inputs
                .borrow_mut()
                .push(serde_json::to_string(requests).unwrap());
            let reply = self
                .replies
                .borrow_mut()
                .pop_front()
                .expect("unexpected retry");
            self.interruption.set(match &reply {
                Err(crate::adapter::BackendError::Message(message)) if message == "interrupted" => {
                    Some(1)
                }
                _ => None,
            });
            reply
        })
    }
}
fn failure(
    status: u16,
    kind: crate::transport::TransportKind,
    seconds: u64,
) -> crate::adapter::BackendError {
    crate::adapter::BackendError::Transport(TransportFailure {
        status: Some(status),
        kind,
        origin: "status",
        url_class: None,
        body_prefix: None,
        body_bytes: 0,
        retry_after: Some(Duration::from_secs(seconds)),
        observation: Default::default(),
    })
}
fn run(
    replies: Vec<Result<LlmResult, crate::adapter::BackendError>>,
    budget: Option<Duration>,
) -> (
    Result<CompletedRun, AgentError>,
    Vec<String>,
    Lifecycle,
    Duration,
) {
    let root = tempfile::tempdir().unwrap();
    let id = SessionId::parse("rate-limit").unwrap();
    let store = SessionStore::load_at(&id, root.path()).unwrap();
    let mut reserved = store.start_request(None, None, "P", root.path()).unwrap();
    let inputs = Rc::new(RefCell::new(Vec::new()));
    let agent = CodingAgent::with_backend(
        Box::new(Script {
            replies: RefCell::new(replies.into()),
            inputs: inputs.clone(),
            interruption: Cell::new(None),
        }),
        root.path().into(),
        false,
    )
    .with_turn_time(budget);
    let turn = Turn::new(&agent).unwrap();
    turn.runtime
        .as_ref()
        .unwrap()
        .block_on(async { tokio::time::pause() });
    let _entered = turn.runtime.as_ref().unwrap().enter();
    let start = tokio::time::Instant::now();
    let result = turn.run_reserved(&store, &mut reserved);
    let elapsed = start.elapsed();
    let captured = inputs.borrow().clone();
    (
        result,
        captured,
        store.snapshot().unwrap().branches[0].lifecycle,
        elapsed,
    )
}
fn healthy() -> LlmResult {
    LlmResult {
        continuation: None,
        text: "recovered".into(),
        thinking: String::new(),
        calls: vec![],
        finish_reason: Some(serdes_ai::core::FinishReason::Stop),
        usage: LlmUsage::default(),
    }
}
#[test]
fn jitter_and_provider_minimum_are_bounded() {
    for retry in 0..MAX_RETRIES {
        let half = 250 << retry;
        assert_eq!(delay(retry, None, 0), Some(Duration::from_millis(half)));
        assert_eq!(
            delay(retry, None, half),
            Some(Duration::from_millis(half * 2))
        );
        assert!(delay(retry, Some(Duration::from_secs(7)), 0).unwrap() >= Duration::from_secs(7));
    }
    assert_eq!(delay(0, Some(MAX_DELAY), u64::MAX), Some(MAX_DELAY));
    assert_eq!(delay(0, Some(Duration::from_secs(u64::MAX)), 0), None);
}
#[test]
fn retry_after_recovers_with_identical_request_and_no_failed_round() {
    let (result, inputs, lifecycle, elapsed) = run(
        vec![
            Err(failure(429, crate::transport::TransportKind::RateLimit, 7)),
            Ok(healthy()),
        ],
        None,
    );
    let recovered = result.unwrap();
    assert_eq!(recovered.summary, "recovered");
    assert_eq!(recovered.retry_attempts, 1);
    assert_eq!(inputs.len(), 2);
    assert_eq!(inputs[0], inputs[1]);
    assert_eq!(lifecycle, Lifecycle::Completed);
    assert!(elapsed >= Duration::from_secs(7));
}
#[test]
fn exhaustion_reports_used_and_remaining_budget() {
    let (result, inputs, lifecycle, _) = run(
        (0..4)
            .map(|_| Err(failure(429, crate::transport::TransportKind::RateLimit, 0)))
            .collect(),
        None,
    );
    let error = result.unwrap_err();
    assert_eq!(error.envelope_code, Some("model-rate-limit"));
    assert_eq!(error.retry_attempts, Some(3));
    assert!(error.message.contains("retry cap exhausted"));
    assert!(error.message.contains("3/3 used, 0 remaining"));
    assert_eq!(inputs.len(), 4);
    assert_eq!(lifecycle, Lifecycle::Failed);
}
#[test]
fn oversized_hint_and_deadline_never_send_an_early_retry() {
    let (result, inputs, _, elapsed) = run(
        vec![Err(failure(
            429,
            crate::transport::TransportKind::RateLimit,
            u64::MAX,
        ))],
        None,
    );
    assert!(result
        .unwrap_err()
        .message
        .contains("0/3 used, 3 remaining"));
    assert_eq!(inputs.len(), 1);
    assert_eq!(elapsed, Duration::ZERO);
    let (result, inputs, _, elapsed) = run(
        vec![Err(failure(
            429,
            crate::transport::TransportKind::RateLimit,
            7,
        ))],
        Some(Duration::from_secs(1)),
    );
    let error = result.unwrap_err();
    assert_eq!(error.key, "turn-time-exhausted");
    assert_eq!(error.retry_attempts, Some(0));
    assert_eq!(inputs.len(), 1);
    assert!(elapsed < Duration::from_secs(2));
}
#[test]
fn unrelated_and_nonretryable_failures_stay_terminal() {
    use crate::transport::TransportKind::*;
    for (status, kind) in [
        (429, QuotaExhausted),
        (401, Permanent),
        (400, Permanent),
        (400, RateLimit),
    ] {
        let (result, inputs, _, elapsed) = run(vec![Err(failure(status, kind, 7))], None);
        assert!(result.is_err());
        assert_eq!(inputs.len(), 1);
        assert_eq!(elapsed, Duration::ZERO);
    }
}

#[test]
fn mixed_stream_loss_and_rate_limit_share_the_reissue_cap() {
    let (result, inputs, _, _) = run(
        vec![
            Err("interrupted".into()),
            Err(failure(429, crate::transport::TransportKind::RateLimit, 0)),
            Err("interrupted".into()),
            Err(failure(429, crate::transport::TransportKind::RateLimit, 0)),
        ],
        None,
    );
    assert!(result
        .unwrap_err()
        .message
        .contains("3/3 used, 0 remaining"));
    assert_eq!(inputs.len(), 4);
}
#[test]
fn permanent_failure_after_a_throttle_stops_immediately() {
    let (result, inputs, _, _) = run(
        vec![
            Err(failure(429, crate::transport::TransportKind::RateLimit, 0)),
            Err(failure(401, crate::transport::TransportKind::Permanent, 0)),
        ],
        None,
    );
    assert_eq!(result.unwrap_err().envelope_code, Some("model-permanent"));
    assert_eq!(inputs.len(), 2);
}

#[test]
fn typed_transient_failures_share_backoff_and_budget() {
    use crate::transport::TransportKind::*;
    let connect = TransportFailure {
        kind: Connectivity,
        status: None,
        origin: "connect",
        url_class: None,
        body_prefix: None,
        body_bytes: 0,
        retry_after: None,
        observation: Default::default(),
    };
    let (result, inputs, lifecycle, elapsed) = run(
        vec![
            Err(failure(503, TransientServer, 2)),
            Err(crate::adapter::BackendError::Transport(connect.clone())),
            Ok(healthy()),
        ],
        None,
    );
    assert_eq!(result.unwrap().retry_attempts, 2);
    assert_eq!(inputs.len(), 3);
    assert!(inputs.iter().all(|request| request == &inputs[0]));
    assert_eq!(lifecycle, Lifecycle::Completed);
    assert!(elapsed >= Duration::from_secs(2));
    let (result, inputs, _, _) = run(
        (0..4)
            .map(|_| Err(crate::adapter::BackendError::Transport(connect.clone())))
            .collect(),
        None,
    );
    let error = result.unwrap_err();
    assert_eq!(error.envelope_code, Some("model-connectivity"));
    assert_eq!(error.retry_attempts, Some(3));
    assert_eq!(inputs.len(), 4);
    assert!(error.message.contains("retry cap exhausted"));
}

#[test]
fn provider_prose_cannot_authorize_retries() {
    let (result, inputs, _, elapsed) = run(
        vec![Err(
            "model transport failed (status 429, origin status, class rate-limit, retryable)"
                .into(),
        )],
        None,
    );
    let error = result.unwrap_err();
    assert_eq!(error.envelope_code, None);
    assert_eq!(error.retry_attempts, Some(0));
    assert_eq!(inputs.len(), 1);
    assert_eq!(elapsed, Duration::ZERO);
}

#[test]
fn mixed_stream_and_transport_failures_cannot_multiply_the_request_cap() {
    use crate::transport::TransportKind::*;
    let (result, inputs, lifecycle, _) = run(
        vec![
            Err(failure(503, TransientServer, 0)),
            Err("interrupted".into()),
            Err(failure(429, RateLimit, 0)),
            Err(failure(503, TransientServer, 0)),
        ],
        None,
    );
    let error = result.unwrap_err();
    assert_eq!(inputs.len(), 4);
    assert!(inputs.iter().all(|request| request == &inputs[0]));
    assert_eq!(error.retry_attempts, Some(3));
    assert_eq!(error.envelope_code, Some("model-transient-server"));
    assert_eq!(lifecycle, Lifecycle::Failed);
}

#[test]
fn accepted_or_ambiguous_transport_loss_is_terminal_without_backoff() {
    for origin in ["request", "body", "decode", "timeout", "unknown"] {
        let failure = TransportFailure {
            kind: crate::transport::TransportKind::Connectivity,
            status: None,
            origin,
            url_class: None,
            body_prefix: None,
            body_bytes: 0,
            retry_after: None,
            observation: Default::default(),
        };
        let (result, inputs, lifecycle, elapsed) = run(
            vec![Err(crate::adapter::BackendError::Transport(failure))],
            None,
        );
        let error = result.unwrap_err();
        assert_eq!(error.envelope_code, Some("model-connectivity"));
        assert_eq!(error.retry_attempts, Some(0));
        assert!(error.message.contains("request not replayed"));
        assert_eq!(inputs.len(), 1);
        assert_eq!(lifecycle, Lifecycle::Failed);
        assert_eq!(elapsed, Duration::ZERO);
    }
}

#[test]
fn one_second_hint_keeps_the_backoff_bounds() {
    let (result, inputs, lifecycle, elapsed) = run(
        vec![
            Err(failure(429, crate::transport::TransportKind::RateLimit, 1)),
            Ok(healthy()),
        ],
        None,
    );
    assert_eq!(result.unwrap().retry_attempts, 1);
    assert_eq!(inputs.len(), 2);
    assert_eq!(inputs[0], inputs[1]);
    assert_eq!(lifecycle, Lifecycle::Completed);
    // Moved unchanged from the compiled loopback test: OS scheduling is not
    // backoff. This measures the actual Turn retry executor's virtual clock.
    assert!(elapsed >= Duration::from_secs(1));
    assert!(elapsed < Duration::from_secs(5));
}
