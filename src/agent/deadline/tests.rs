use super::*;
use crate::adapter::{LlmUsage, ModelFuture};
use crate::session::{Lifecycle, SessionId};
use serdes_ai::core::FinishReason;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;

struct Reply {
    delay: Duration,
    result: Result<LlmResult, crate::adapter::ModelFailure>,
}

struct Script {
    replies: RefCell<VecDeque<Reply>>,
    active: Rc<Cell<usize>>,
    calls: Cell<usize>,
}

struct InFlight(Rc<Cell<usize>>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

impl ChatBackend for Script {
    fn request<'a>(
        &'a self,
        _: &'a [serdes_ai::core::ModelRequest],
        _: &'a [crate::tools::ToolSpec],
    ) -> ModelFuture<'a> {
        Box::pin(async move {
            self.calls.set(self.calls.get() + 1);
            self.active.set(self.active.get() + 1);
            let _guard = InFlight(self.active.clone());
            let reply = self
                .replies
                .borrow_mut()
                .pop_front()
                .expect("script exhausted");
            tokio::time::sleep(reply.delay).await;
            reply.result
        })
    }

    fn request_calls(&self) -> usize {
        self.calls.get()
    }
}

fn reply(seconds: u64, text: &str, calls: Vec<ToolCall>) -> Reply {
    Reply {
        delay: Duration::from_secs(seconds),
        result: Ok(LlmResult {
            text: text.into(),
            finish_reason: Some(if calls.is_empty() {
                FinishReason::Stop
            } else {
                FinishReason::ToolCall
            }),
            calls,
            usage: LlmUsage::default(),
        }),
    }
}

fn list() -> ToolCall {
    ToolCall {
        id: "list-1".into(),
        name: "list_directory".into(),
        args_json: r#"{"path":"."}"#.into(),
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    agent: CodingAgent,
    store: SessionStore,
    reserved: ReservedRequest,
    active: Rc<Cell<usize>>,
}

impl Fixture {
    fn new(replies: Vec<Reply>, budget: Option<u64>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let active = Rc::new(Cell::new(0));
        let agent = CodingAgent::with_backend(
            Box::new(Script {
                replies: RefCell::new(replies.into()),
                active: active.clone(),
                calls: Cell::new(0),
            }),
            root.path().to_path_buf(),
            false,
        )
        .with_turn_time(budget.map(Duration::from_secs));
        let store =
            SessionStore::load_at(&SessionId::parse("deadline").unwrap(), root.path()).unwrap();
        let reserved = store.start_request(None, None, "P", root.path()).unwrap();
        Self {
            _root: root,
            agent,
            store,
            reserved,
            active,
        }
    }

    fn run(&mut self) -> Result<CompletedRun, AgentError> {
        let lease_reservation = self.reserved.clone();
        let turn = Turn::new(&self.agent, &self.store, &lease_reservation).unwrap();
        // Auto-advance a single turn's executor, not a fresh clock per request.
        turn.runtime.block_on(async { tokio::time::pause() });
        let _entered = turn.runtime.enter();
        let result = turn.run_counted(&self.store, &mut self.reserved);
        if let Some(budget) = self.agent.turn_time_budget {
            assert!(
                turn.started.elapsed() < budget + Duration::from_secs(1),
                "turn exceeded its single deadline: {:?}",
                turn.started.elapsed()
            );
        }
        result
    }

    fn assert_timeout(&mut self, calls: usize, rounds: usize) {
        let error = self.run().unwrap_err();
        assert_eq!(error.key, "turn-time-exhausted");
        assert_eq!(error.code, crate::envelope::Code::Model);
        assert!(error
            .message
            .contains("raise --turn-time or pass 0 to disable"));
        assert_eq!(self.agent.model_calls(), calls);
        assert_eq!(self.active.get(), 0, "cancelled request must be dropped");
        let snapshot = self.store.snapshot().unwrap();
        let branch = &snapshot.branches[0];
        assert_eq!(branch.lifecycle, Lifecycle::Failed);
        assert_eq!(branch.rounds.len(), rounds);
        assert!(!self
            .store
            .session_dir()
            .join("tool-in-flight.json")
            .exists());
        let retry = self
            .store
            .start_request(None, None, "P", self._root.path())
            .unwrap();
        assert!(
            retry.retry,
            "timeout must release the reservation for retry"
        );
        assert!(!retry.replay);
    }
}

#[test]
fn initial_request_with_tools_is_interrupted_before_side_effects() {
    let mut f = Fixture::new(vec![reply(20, "list", vec![list()])], Some(10));
    f.assert_timeout(1, 0);
}

#[test]
fn initial_no_tools_response_is_interrupted() {
    let mut f = Fixture::new(vec![reply(20, "OK", vec![])], Some(10));
    f.assert_timeout(1, 0);
}

#[test]
fn subsequent_no_tools_response_uses_remaining_turn_time() {
    // Each request fits 10s individually; together they do not. A per-call reset fails.
    let mut f = Fixture::new(
        vec![reply(6, "list", vec![list()]), reply(6, "OK", vec![])],
        Some(10),
    );
    f.assert_timeout(2, 1);
}

#[test]
fn forced_summary_uses_the_same_deadline() {
    let mut f = Fixture::new(vec![reply(6, "", vec![]), reply(6, "OK", vec![])], Some(10));
    f.assert_timeout(2, 0);
}

#[test]
fn truncation_retry_does_not_restart_the_clock() {
    let mut first = reply(6, "truncated", vec![]);
    first.result.as_mut().unwrap().finish_reason = Some(FinishReason::Length);
    let mut f = Fixture::new(vec![first, reply(6, "OK", vec![])], Some(10));
    f.assert_timeout(2, 0);
}

#[test]
fn provider_context_retry_does_not_restart_the_clock() {
    let first = Reply {
        delay: Duration::from_secs(6),
        result: Err(crate::adapter::ModelFailure::from_model_error(
            serdes_ai::models::ModelError::ContextLengthExceeded {
                max_tokens: 1,
                requested_tokens: 2,
            },
        )),
    };
    let mut f = Fixture::new(vec![first, reply(6, "OK", vec![])], Some(10));
    f.assert_timeout(2, 0);
}

#[test]
fn normal_completion_and_replay_do_not_cancel_or_request_again() {
    let mut f = Fixture::new(vec![reply(1, "OK", vec![])], Some(10));
    assert_eq!(f.run().unwrap().summary, "OK");
    assert_eq!(f.active.get(), 0);
    let replay = f
        .store
        .start_request(Some(1), None, "P", f._root.path())
        .unwrap();
    assert!(f.agent.run(&f.store, &replay).unwrap().replayed);
    assert_eq!(f.agent.model_calls(), 1);
}

#[test]
fn disabled_turn_limit_allows_a_long_response() {
    let mut f = Fixture::new(vec![reply(100, "OK", vec![])], None);
    assert_eq!(f.run().unwrap().summary, "OK");
    assert_eq!(f.active.get(), 0);
}

#[test]
fn provider_timeout_keeps_its_own_failure_instead_of_turn_budget() {
    let mut f = Fixture::new(
        vec![Reply {
            delay: Duration::from_secs(1),
            result: Err(crate::adapter::ModelFailure::Terminal(
                "responses request exceeded the configured timeout".into(),
            )),
        }],
        Some(10),
    );
    let error = f.run().unwrap_err();
    assert_eq!(error.key, "model");
    assert_eq!(f.active.get(), 0);
}

mod retries;
