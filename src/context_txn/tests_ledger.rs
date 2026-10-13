use super::{
    bound_port::BoundPort,
    budget::{Budget, Margins},
    executor::*,
    ledger::LedgerCommitError,
    operation::{self, Proposer},
};
use crate::context_kernel::{
    events::EventKind,
    ledger::tests::{empirical, obligation},
    ledger::*,
};
use std::collections::BTreeMap;
use std::rc::Rc;
struct Receipts {
    accepted: bool,
}
impl LedgerPorts for Receipts {
    fn authenticated(&self, source: &Source) -> bool {
        self.accepted && source.leaf.starts_with("ingress:")
    }
    fn gate_supported(&self, support: &GateSupport) -> bool {
        self.accepted && !support.ingress_leaves.is_empty()
    }
    fn tool_write_authenticated(&self, result: &ToolWrite, _: &BTreeMap<String, u64>) -> bool {
        self.accepted && result.tool == "write_file"
    }
}
fn executor() -> (Executor, FencingClock) {
    let clock = FencingClock::new();
    let mut executor = Executor::new_generous(clock.acquire());
    executor.bind_port(Rc::new(BoundPort::new(Margins::V1, 0, 0)));
    executor.ingest_test_item();
    (executor, clock)
}
fn prepare(executor: &mut Executor, command: &Command, by: Proposer) -> u64 {
    executor.propose_as(command.operation(), 1, by).unwrap();
    executor.snapshot().unwrap();
    executor.generate().unwrap();
    executor
        .validate_ledger(
            command,
            &Budget {
                b: 1 << 20,
                r: 8,
                h: 4,
            },
        )
        .unwrap()
}
#[test]
fn phase5_registry_commands_have_exact_proposer_and_cannot_be_controller_written() {
    for (name, proposer) in [
        ("admit-obligation", Proposer::S),
        ("admit-convenience", Proposer::S),
        ("demote", Proposer::L),
        ("discharge", Proposer::L),
        ("consolidate-obligations", Proposer::L),
        ("amend", Proposer::M),
        ("retract", Proposer::M),
        ("promote", Proposer::M),
        ("fact-invalidate", Proposer::S),
        ("revalidate", Proposer::L),
        ("stale-demote", Proposer::S),
        ("decay", Proposer::S),
    ] {
        assert_eq!(operation::find(name).unwrap().proposer, proposer);
        let (mut executor, _) = executor();
        assert!(matches!(
            executor.propose_as(name, 1, Proposer::C),
            Err(ExecutorError::AuthorityDenied { .. })
        ));
    }
}
#[test]
fn authenticated_admission_and_controller_refusal() {
    let (mut executor, clock) = executor();
    let entry = obligation("must remain");
    let command = Command::AdmitObligation {
        entry: entry.clone(),
        grammar: Grammar::Obligation,
    };
    let bound = prepare(&mut executor, &command, Proposer::S);
    assert_eq!(
        executor.commit_ledger(1, &clock, command, &Receipts { accepted: true }, bound),
        Ok(CommitOutcome::Applied)
    );
    assert!(executor.ledger().ledger_active(&entry.id));
    assert!(matches!(
        executor.propose_as("discharge", 1, Proposer::C),
        Err(ExecutorError::AuthorityDenied { .. })
    ));
    assert!(executor
        .ledger()
        .request_retirement(
            &RetirementRequest {
                fact: entry.id.clone(),
                budget_justification: "need space".into()
            },
            &Receipts { accepted: true }
        )
        .is_none());
}
#[test]
fn forged_ingress_and_generic_commit_are_rejected_without_mutation() {
    let (mut executor, clock) = executor();
    let command = Command::AdmitObligation {
        entry: obligation("forged"),
        grammar: Grammar::Obligation,
    };
    let bound = prepare(&mut executor, &command, Proposer::S);
    let before = executor.chain_head();
    assert_eq!(
        executor.commit_ledger(
            1,
            &clock,
            command.clone(),
            &Receipts { accepted: false },
            bound
        ),
        Err(LedgerCommitError::Domain(LedgerError::Authority))
    );
    assert_eq!(executor.chain_head(), before);
    let bound = prepare(&mut executor, &command, Proposer::S);
    assert_eq!(
        executor.commit(
            1,
            Some(EventKind::Ledger {
                command: Box::new(command)
            }),
            bound
        ),
        Err(ExecutorError::PreconditionFailed {
            which: "ledger-receipt-boundary"
        })
    );
    assert_eq!(executor.chain_head(), before);
}
#[test]
fn fencing_stale_parent_and_overflow_do_not_publish_ledger() {
    let (mut executor, clock) = executor();
    let command = Command::AdmitObligation {
        entry: obligation("protected"),
        grammar: Grammar::Obligation,
    };
    let bound = prepare(&mut executor, &command, Proposer::S);
    let before = executor.chain_head();
    assert!(matches!(
        executor.commit_ledger(
            2,
            &clock,
            command.clone(),
            &Receipts { accepted: true },
            bound
        ),
        Err(LedgerCommitError::Transaction(
            ExecutorError::StaleParent { .. }
        ))
    ));
    assert_eq!(executor.chain_head(), before);
    let bound = prepare(&mut executor, &command, Proposer::S);
    executor.arm_region_accounting(0);
    assert!(matches!(
        executor.commit_ledger(
            1,
            &clock,
            command.clone(),
            &Receipts { accepted: true },
            bound
        ),
        Err(LedgerCommitError::Transaction(
            ExecutorError::RegionOverBudget { .. }
        ))
    ));
    assert_eq!(executor.chain_head(), before);
    let bound = prepare(&mut executor, &command, Proposer::S);
    clock.acquire();
    assert!(matches!(
        executor.commit_ledger(1, &clock, command, &Receipts { accepted: true }, bound),
        Err(LedgerCommitError::Transaction(ExecutorError::Fenced { .. }))
    ));
    assert_eq!(executor.chain_head(), before);
}
#[test]
fn write_result_and_invalidation_are_one_atomic_sequenced_effect() {
    let (mut executor, clock) = executor();
    let entry = empirical("premise");
    let command = Command::AdmitConvenience {
        entry: entry.clone(),
    };
    let bound = prepare(&mut executor, &command, Proposer::S);
    executor
        .commit_ledger(1, &clock, command, &Receipts { accepted: true }, bound)
        .unwrap();
    let before = executor.chain_head();
    let command = Command::FactInvalidate {
        result: ToolWrite {
            call_id: "call-1".into(),
            tool: "write_file".into(),
            scope: 1,
            sanitized: b"changed workspace".to_vec(),
        },
        writes: BTreeMap::from([("workspace".into(), 2)]),
    };
    let bound = prepare(&mut executor, &command, Proposer::S);
    executor
        .commit_ledger(1, &clock, command, &Receipts { accepted: true }, bound)
        .unwrap();
    assert_eq!(executor.chain_head().0, before.0 + 1);
    assert_eq!(
        executor.ledger().entry(&entry.id).unwrap().freshness,
        Freshness::Stale
    );
    let before = executor.chain_head();
    let prior = executor.ledger().clone();
    let command = Command::FactInvalidate {
        result: ToolWrite {
            call_id: "bad-result".into(),
            tool: "write_file".into(),
            scope: 404,
            sanitized: b"should not append".to_vec(),
        },
        writes: BTreeMap::from([("workspace".into(), 3)]),
    };
    executor
        .propose_as(command.operation(), 1, Proposer::S)
        .unwrap();
    executor.snapshot().unwrap();
    executor.generate().unwrap();
    assert!(matches!(
        executor.validate_ledger(
            &command,
            &Budget {
                b: 1 << 20,
                r: 8,
                h: 4
            }
        ),
        Err(LedgerCommitError::Replay(_))
    ));
    assert_eq!(executor.chain_head(), before);
    assert_eq!(executor.ledger(), &prior);
}

#[test]
fn ledger_validation_binds_actual_command_bytes_and_protection_floor() {
    let (mut executor, clock) = executor();
    let command = Command::AdmitObligation {
        entry: obligation("protected"),
        grammar: Grammar::Obligation,
    };
    let bound = prepare(&mut executor, &command, Proposer::S);
    let changed = Command::AdmitObligation {
        entry: obligation("other bytes"),
        grammar: Grammar::Obligation,
    };
    let before = executor.chain_head();
    assert_eq!(
        executor.commit_ledger(1, &clock, changed, &Receipts { accepted: true }, bound),
        Err(LedgerCommitError::Boundary)
    );
    assert_eq!(executor.chain_head(), before);
    executor
        .propose_as(command.operation(), 1, Proposer::S)
        .unwrap();
    executor.snapshot().unwrap();
    executor.generate().unwrap();
    assert!(matches!(
        executor.validate_ledger(&command, &Budget { b: 1, r: 0, h: 0 }),
        Err(LedgerCommitError::Transaction(
            ExecutorError::PreconditionFailed {
                which: "protection-floor"
            }
        ))
    ));
    assert_eq!(executor.chain_head(), before);
}
#[test]
fn untyped_validation_cannot_authorize_a_ledger_effect() {
    let (mut executor, clock) = executor();
    let command = Command::AdmitObligation {
        entry: obligation("bound"),
        grammar: Grammar::Obligation,
    };
    executor
        .propose_as(command.operation(), 1, Proposer::S)
        .unwrap();
    executor.snapshot().unwrap();
    executor.generate().unwrap();
    let bound = executor.port_bound(0, 1).unwrap();
    executor
        .validate(
            bound,
            &Budget {
                b: 1 << 20,
                r: 8,
                h: 4,
            },
            1,
            0,
            0,
            1,
        )
        .unwrap();
    assert_eq!(
        executor.commit_ledger(1, &clock, command, &Receipts { accepted: true }, bound),
        Err(LedgerCommitError::Boundary)
    );
}

#[test]
fn missing_accounting_port_aborts_ledger_validation() {
    let clock = FencingClock::new();
    let mut executor = Executor::new_generous(clock.acquire());
    executor.ingest_test_item();
    let command = Command::AdmitObligation {
        entry: obligation("unaccounted"),
        grammar: Grammar::Obligation,
    };
    executor
        .propose_as(command.operation(), 1, Proposer::S)
        .unwrap();
    executor.snapshot().unwrap();
    executor.generate().unwrap();
    let before = executor.chain_head();
    assert_eq!(
        executor.validate_ledger(
            &command,
            &Budget {
                b: 1 << 20,
                r: 8,
                h: 4
            }
        ),
        Err(LedgerCommitError::Boundary)
    );
    assert_eq!(executor.state(), TxnState::Aborted);
    assert_eq!(executor.chain_head(), before);
}
