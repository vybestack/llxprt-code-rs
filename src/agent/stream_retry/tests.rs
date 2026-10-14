use super::*;
use crate::adapter::{LlmUsage, ModelFuture};
use crate::session::{Lifecycle, SessionId};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

struct Script {
    replies: RefCell<VecDeque<Result<LlmResult, Option<usize>>>>,
    interruption: Cell<Option<usize>>,
    calls: Cell<usize>,
    inputs: std::rc::Rc<RefCell<Vec<String>>>,
}
impl ChatBackend for Script {
    fn stream_interruption(&self) -> Option<usize> {
        self.interruption.get()
    }
    fn request_calls(&self) -> usize {
        self.calls.get()
    }
    fn request<'a>(
        &'a self,
        requests: &'a [serdes_ai::core::ModelRequest],
        _: &'a [crate::tools::ToolSpec],
    ) -> ModelFuture<'a> {
        Box::pin(async move {
            self.interruption.set(None);
            self.calls.set(self.calls.get() + 1);
            self.inputs
                .borrow_mut()
                .push(serde_json::to_string(requests).unwrap());
            match self.replies.borrow_mut().pop_front().unwrap() {
                Ok(reply) => Ok(reply),
                Err(events) => {
                    self.interruption.set(events);
                    Err("model failure".into())
                }
            }
        })
    }
}
fn reply(calls: Vec<ToolCall>) -> LlmResult {
    LlmResult {
        continuation: None,
        text: "healthy".into(),
        thinking: String::new(),
        finish_reason: Some(if calls.is_empty() {
            serdes_ai::core::FinishReason::Stop
        } else {
            serdes_ai::core::FinishReason::ToolCall
        }),
        calls,
        usage: LlmUsage::default(),
    }
}
fn run(
    script: Vec<Result<LlmResult, Option<usize>>>,
    budget_ms: Option<u64>,
) -> (
    Result<CompletedRun, AgentError>,
    usize,
    Vec<crate::session::StreamRetryRecord>,
    Vec<String>,
    Lifecycle,
) {
    let root = tempfile::tempdir().unwrap();
    let store = SessionStore::load_at(&SessionId::parse("retry").unwrap(), root.path()).unwrap();
    let mut reserved = store.start_request(None, None, "P", root.path()).unwrap();
    let inputs = std::rc::Rc::new(RefCell::new(Vec::new()));
    let agent = CodingAgent::with_backend(
        Box::new(Script {
            replies: RefCell::new(script.into()),
            interruption: Cell::new(None),
            calls: Cell::new(0),
            inputs: inputs.clone(),
        }),
        root.path().into(),
        false,
    )
    .with_turn_time(budget_ms.map(std::time::Duration::from_millis));
    let turn = Turn::new(&agent).unwrap();
    // Use the production per-turn executor with virtual backoff time.
    let rt = turn.runtime.as_ref().unwrap();
    rt.block_on(async { tokio::time::pause() });
    let _entered = rt.enter();
    let result = turn.run_reserved(&store, &mut reserved);
    let calls = agent.model_calls();
    // Reopen through the read-only path to prove retry evidence survives process state.
    let state =
        SessionStore::read_transcript_at(&SessionId::parse("retry").unwrap(), root.path()).unwrap();
    let branch = &state.branches[0];
    // Request bytes are also asserted by the real HTTP regression.
    let captured = inputs.borrow().clone();
    (
        result,
        calls,
        branch.stream_retries.clone(),
        captured,
        branch.lifecycle,
    )
}
#[test]
fn bounded_backoff_exhaustion_keeps_partial_response_a_failure() {
    let (result, calls, records, _, lifecycle) = run(
        vec![Err(Some(7)), Err(Some(8)), Err(Some(9)), Err(Some(10))],
        None,
    );
    let error = result.unwrap_err();
    assert!(error.message.contains("3 retries exhausted"));
    assert!(error.message.contains("10 stream events"));
    assert_eq!(calls, 4);
    assert_eq!(lifecycle, Lifecycle::Failed);
    assert_eq!(
        records
            .iter()
            .map(|r| (r.retry, r.delay_ms, r.events))
            .collect::<Vec<_>>(),
        vec![(1, 250, 7), (2, 500, 8), (3, 1000, 9)]
    );
}
#[test]
fn successful_reissue_and_permanent_errors_are_distinct() {
    let (result, calls, records, inputs, lifecycle) =
        run(vec![Err(Some(12)), Ok(reply(vec![]))], None);
    assert_eq!(inputs[0], inputs[1]);
    assert_eq!(result.unwrap().summary, "healthy");
    assert_eq!(calls, 2);
    assert_eq!(records.len(), 1);
    assert_eq!(lifecycle, Lifecycle::Completed);
    let (result, calls, records, _, _) = run(vec![Err(None)], None);
    assert!(result.is_err());
    assert_eq!(calls, 1);
    assert!(records.is_empty());
}
#[test]
fn original_deadline_bounds_backoff_without_sending_another_request() {
    let (result, calls, records, _, _) = run(vec![Err(Some(3))], Some(100));
    assert_eq!(result.unwrap_err().key, "turn-time-exhausted");
    assert_eq!(calls, 1);
    assert_eq!(records.len(), 1);
}

#[test]
fn transport_retry_after_tool_checkpoint_keeps_prior_results_without_reexecuting_tools() {
    let tool = ToolCall {
        id: "write".into(),
        name: "write_file".into(),
        args_json: r#"{"path":"once","content":"checkpointed"}"#.into(),
    };
    let (result, calls, records, inputs, lifecycle) = run(
        vec![Ok(reply(vec![tool])), Err(Some(4)), Ok(reply(vec![]))],
        None,
    );
    assert_eq!(result.unwrap().tool_count, 1);
    assert_eq!(lifecycle, Lifecycle::Completed);
    assert_eq!(calls, 3);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].round, 2);
    assert_eq!(inputs[1], inputs[2]);
    assert!(inputs[2].contains("checkpointed"));
}

#[test]
fn refusal_to_persist_retry_stops_before_any_reissue() {
    let root = tempfile::tempdir().unwrap();
    let store = SessionStore::load_at(&SessionId::parse("persist").unwrap(), root.path()).unwrap();
    let reserved = store.start_request(None, None, "P", root.path()).unwrap();
    let agent = CodingAgent::with_backend(
        Box::new(Script {
            replies: RefCell::new(vec![Err(Some(8))].into()),
            interruption: Cell::new(None),
            calls: Cell::new(0),
            inputs: std::rc::Rc::new(RefCell::new(Vec::new())),
        }),
        root.path().into(),
        false,
    );
    store.fail(&reserved, "already terminal", &[]).unwrap();
    let turn = Turn::new(&agent).unwrap();
    assert!(matches!(
        turn.retry_stream(&[], &[], (&store, &reserved, 1)),
        Err(RoundFailure::RetryStore(_))
    ));
    // Gateway checks durability and the live lease BEFORE the initial send.
    assert_eq!(agent.model_calls(), 0);
    assert!(store.snapshot().unwrap().branches[0]
        .stream_retries
        .is_empty());
}
