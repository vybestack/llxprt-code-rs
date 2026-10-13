//! Receipt-bound effects, note flushing, reclaim, provenance and cold replay.
use llxprt_code_rs::{
    context_ingress::ingress::IngressTxn,
    context_kernel::{
        events::{EventKind, EventLog, Sequencer},
        ledger::{Authority, Origin, Source},
        migration::V2,
        reducer::{Reducer, IDLENESS_WINDOW},
        validation::*,
    },
    context_store::{store::ContextStore, vault::VaultKey},
    context_txn::{
        bound_port::BoundPort,
        budget::{Budget, Margins},
        executor::{Executor, FencingClock},
        operation::Proposer,
        semantic::{SemanticError, SemanticServices},
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};
fn independence() -> Independence {
    Independence {
        compactor: "compact-a".into(),
        scorer: "score-b".into(),
    }
}
struct Store;
impl LeafStore for Store {
    fn authenticated_leaf(&self, id: &str) -> Option<Leaf> {
        (id == "source").then(|| Leaf {
            id: id.into(),
            source: Source {
                leaf: id.into(),
                authority: Authority::User,
                origin: Origin::Ingress,
                scope: 1,
            },
            sanitized:
                "supported detail padding padding padding padding padding padding padding padding"
                    .into(),
        })
    }
}
#[derive(Default)]
struct Consumers {
    fail: bool,
}
impl GateConsumers for Consumers {
    fn independence(&self) -> Independence {
        independence()
    }
    fn required_facts(&mut self, _: &[BoundFact]) -> Result<Vec<BoundFact>, GateError> {
        Ok(vec![])
    }
    fn assertions(&mut self, text: &str) -> Result<Vec<String>, GateError> {
        Ok(vec![text.into()])
    }
    fn score(&mut self, text: &str, leaves: &[Leaf], _: &[BoundFact]) -> Result<Score, GateError> {
        if self.fail {
            return Err(GateError::Consumer("scorer"));
        }
        Ok(Score {
            entailment: if leaves.iter().any(|l| l.sanitized.contains(text)) {
                10_000
            } else {
                0
            },
            contradiction: false,
        })
    }
}
fn budget() -> Budget {
    Budget {
        b: 1 << 20,
        r: 8,
        h: 4,
    }
}
fn executor() -> (Executor, FencingClock) {
    let clock = FencingClock::new();
    let mut ex = Executor::new_generous(clock.acquire());
    ex.bind_port(Rc::new(BoundPort::new(Margins::V1, 0, 0)));
    (ex, clock)
}
fn command(op: SemanticOperation, key: &str, text: &str, sources: &[&str]) -> SemanticCommand {
    SemanticCommand {
        operation: op,
        key: key.into(),
        sources: sources.iter().map(|s| (*s).into()).collect(),
        lane: 3,
        store_handle: String::new(),
        candidate: Candidate {
            artifact: text.into(),
            rendered_text: text.into(),
            rendered: vec![],
            roots: vec![Pointer::Ingress("source".into())],
            graph: BTreeMap::new(),
            pending_notes: BTreeSet::new(),
            scope: 1,
        },
    }
}
fn prepare(ex: &mut Executor, op: &'static str) {
    let by = llxprt_code_rs::context_txn::operation::find(op)
        .unwrap()
        .proposer;
    ex.propose_as(op, 1, by).unwrap();
    ex.snapshot().unwrap();
    ex.generate().unwrap();
}
fn validate(
    ex: &mut Executor,
    command: SemanticCommand,
    fail: bool,
) -> Result<llxprt_code_rs::context_txn::semantic::SemanticReceipt, SemanticError> {
    let gate = Gate::new(1, independence()).unwrap();
    let mut consumers = Consumers { fail };
    let mut txn = IngressTxn::new(65_536, 1_000_000);
    let mut sink = ContextStore::open(&VaultKey::from([9; 32]));
    ex.validate_semantic(
        command,
        SemanticServices {
            gate: &gate,
            required: &[],
            always_checked: &[],
            store: &Store,
            consumers: &mut consumers,
            ingress: &mut txn,
            sink: &mut sink,
        },
        &budget(),
    )
}
#[test]
fn note_must_flush_before_lossy_commit_and_real_rendered_reclaim_is_computed() {
    let (mut ex, clock) = executor();
    prepare(&mut ex, "note");
    let receipt = validate(
        &mut ex,
        command(
            SemanticOperation::Note,
            "note",
            "supported detail padding padding padding padding padding padding",
            &[],
        ),
        false,
    )
    .unwrap();
    ex.commit_semantic(1, &clock, receipt).unwrap();
    assert!(!ex.semantic().artifacts()["note"].rendered);
    prepare(&mut ex, "compact");
    assert!(validate(
        &mut ex,
        command(SemanticOperation::Compact, "short", "supported", &["note"]),
        false
    )
    .is_err());
    assert!(ex.semantic().artifacts().contains_key("note"));
    prepare(&mut ex, "flush-notes");
    let receipt = ex
        .validate_projection(SemanticEvent::FlushNotes, &budget())
        .unwrap();
    ex.commit_semantic(1, &clock, receipt).unwrap();
    prepare(&mut ex, "compact");
    let receipt = validate(
        &mut ex,
        command(SemanticOperation::Compact, "short", "supported", &["note"]),
        false,
    )
    .unwrap();
    ex.commit_semantic(1, &clock, receipt).unwrap();
    assert!(!ex.semantic().artifacts().contains_key("note"));
    assert_eq!(ex.semantic().artifacts()["short"].depth, 1);
    assert_eq!(
        ex.semantic().artifacts()["short"].validation.verdict,
        Verdict::Pass
    );
    prepare(&mut ex, "regenerate");
    assert!(validate(
        &mut ex,
        command(
            SemanticOperation::Regenerate,
            "same-size",
            "supported",
            &["short"]
        ),
        false
    )
    .is_err());
    assert!(ex.semantic().artifacts().contains_key("short"));
}
#[test]
fn unsupported_scorer_outage_stale_parent_and_fence_preserve_projection() {
    let (mut ex, clock) = executor();
    let before = ex.chain_head();
    for (text, fail) in [("unsupported", false), ("supported", true)] {
        prepare(&mut ex, "read-back");
        assert!(validate(
            &mut ex,
            command(SemanticOperation::ReadBack, "read", text, &[]),
            fail
        )
        .is_err());
        assert_eq!(ex.chain_head(), before);
    }
    prepare(&mut ex, "reopen");
    let receipt = validate(
        &mut ex,
        command(SemanticOperation::Reopen, "read", "supported", &[]),
        false,
    )
    .unwrap();
    assert!(matches!(
        ex.commit_semantic(2, &clock, receipt),
        Err(SemanticError::Transaction(
            llxprt_code_rs::context_txn::executor::ExecutorError::StaleParent {
                expected: 1,
                actual: 2
            }
        ))
    ));
    assert!(ex.semantic().artifacts().is_empty());
    prepare(&mut ex, "reopen");
    let receipt = validate(
        &mut ex,
        command(SemanticOperation::Reopen, "read", "supported", &[]),
        false,
    )
    .unwrap();
    clock.acquire();
    assert!(ex.commit_semantic(1, &clock, receipt).is_err());
    assert_eq!(ex.chain_head(), before);
}
#[test]
fn classifier_can_only_decrease_protection_and_split_has_exact_byte_cover() {
    let (mut ex, clock) = executor();
    prepare(&mut ex, "read-back");
    let receipt = validate(
        &mut ex,
        command(SemanticOperation::ReadBack, "read", "supported detail", &[]),
        false,
    )
    .unwrap();
    ex.commit_semantic(1, &clock, receipt).unwrap();
    prepare(&mut ex, "reclassify");
    assert!(ex
        .validate_projection(
            SemanticEvent::Reclassify {
                key: "read".into(),
                lane: 1
            },
            &budget()
        )
        .is_err());
    prepare(&mut ex, "reclassify");
    let receipt = ex
        .validate_projection(
            SemanticEvent::Reclassify {
                key: "read".into(),
                lane: 4,
            },
            &budget(),
        )
        .unwrap();
    ex.commit_semantic(1, &clock, receipt).unwrap();
    assert_eq!(ex.semantic().artifacts()["read"].command.lane, 4);
    prepare(&mut ex, "resegment");
    assert!(ex
        .validate_projection(
            SemanticEvent::Resegment {
                key: "read".into(),
                parts: vec!["supported".into(), "missing".into()]
            },
            &budget()
        )
        .is_err());
    prepare(&mut ex, "resegment");
    let receipt = ex
        .validate_projection(
            SemanticEvent::Resegment {
                key: "read".into(),
                parts: vec!["supported".into(), " detail".into()],
            },
            &budget(),
        )
        .unwrap();
    ex.commit_semantic(1, &clock, receipt).unwrap();
    assert_eq!(ex.semantic().artifacts().len(), 2);
}
#[test]
fn typed_verdict_and_semantic_projection_survive_cold_and_tail_replay() {
    let mut c = command(SemanticOperation::ReadBack, "read", "supported", &[]);
    c.store_handle = "admitted-handle".into();
    let record = Gate::new(1, independence())
        .unwrap()
        .validate(&c.candidate, &[], &[], &Store, &mut Consumers::default())
        .unwrap();
    let effect = SemanticEvent::Put {
        command: Box::new(c),
        validation: Box::new(record),
    };
    let mut log = EventLog::new(V2);
    let mut sequencer = Sequencer::continuing(&log, 1, 1000);
    log.append(sequencer.append(
        EventKind::Semantic {
            event: Box::new(effect),
        },
        V2,
    ))
    .unwrap();
    let reducer = Reducer::new(IDLENESS_WINDOW);
    let first = reducer.fold(&log).unwrap();
    log.append(sequencer.append(
        EventKind::Semantic {
            event: Box::new(SemanticEvent::FlushNotes),
        },
        V2,
    ))
    .unwrap();
    let cold = reducer.fold(&log).unwrap();
    let mut resumed = first;
    reducer.fold_from(&mut resumed, &log).unwrap();
    assert_eq!(cold, resumed);
    assert_eq!(
        cold.semantic.artifacts()["read"].validation.verdict,
        Verdict::Pass
    );
    let encoded = serde_json::to_vec(&cold.semantic.artifacts()["read"]).unwrap();
    assert!(String::from_utf8(encoded)
        .unwrap()
        .contains("candidate_digest"));
    assert_eq!(
        llxprt_code_rs::context_txn::operation::find("reclassify")
            .unwrap()
            .proposer,
        Proposer::M
    );
}

#[test]
fn receipt_checked_read_back_preserves_explicit_rebase_no_op() {
    let (mut ex, clock) = executor();
    prepare(&mut ex, "read-back");
    let receipt = validate(
        &mut ex,
        command(SemanticOperation::ReadBack, "read", "supported", &[]),
        false,
    )
    .unwrap();
    let before = ex.chain_head();
    assert_eq!(
        ex.commit_semantic(2, &clock, receipt).unwrap(),
        llxprt_code_rs::context_txn::executor::CommitOutcome::RebaseNoOp
    );
    assert_eq!(ex.chain_head(), before);
    assert!(ex.semantic().artifacts().is_empty());
}

#[test]
fn infrastructure_failure_routes_to_deterministic_emergency_never_commit() {
    assert_eq!(
        SemanticError::Gate(GateError::Consumer("scorer")).response(),
        Response::DeterministicEmergency
    );
    let record = ValidationRecord {
        candidate_digest: "x".into(),
        verdict: Verdict::FailCompleteness { missing: vec![] },
        support: BTreeMap::new(),
        cap_pressure: false,
        refreshed: 0,
        required_refinement_unavailable: false,
    };
    assert_eq!(
        SemanticError::Rejected(record).response(),
        Response::RestoreRequired
    );
}
