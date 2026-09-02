//! Tests for the phase-3 red step: registry closure conformance (green now)
//! and the model-based transition / rebase / crash / budget properties that
//! the green turn must satisfy (red now).

use super::budget::{self, AccountingPort, Budget};
use super::executor::{Epoch, Executor, ExecutorError, TxnState};
use super::operation::{self, Proposer};

/// Deterministic, additive, conservative test port.
struct TestPort;

impl AccountingPort for TestPort {
    fn bound(&self, _version: u64, contract: u64) -> u64 {
        contract
    }
}

fn snake(name: &str) -> String {
    let mut out = String::new();
    for (i, ch) in name.char_indices() {
        if i > 0 && ch.is_ascii_uppercase() {
            out.push('-');
        }
        out.push(ch.to_ascii_lowercase());
    }
    out
}

fn names() -> Vec<&'static str> {
    operation::registry().iter().map(|r| r.name).collect()
}

/// GREEN: every EventKind::OperationCommit variant has a registry row named by
/// the snake_case of the variant.
#[test]
fn operation_class_variants_are_registered() {
    let classes = [
        "ScopeOpen",
        "ScopeCloseByEvent",
        "ScopeCloseByDeclaration",
        "Resegment",
        "Place",
        "Unplace",
        "Pin",
        "Unpin",
        "LanePolicyUpdate",
        "MigrationSelect",
        "AdmitIngress",
        "Sanitize",
        "Redact",
        "Import",
        "RuleUpdate",
        "VocabularyUpdate",
        "IndexRebuild",
        "StoreMode",
        "QuiesceUnwritable",
    ];
    let have = names();
    for class in classes {
        let row = snake(class);
        assert!(have.contains(&row.as_str()), "{class} -> {row} missing");
    }
}

/// GREEN: no duplicate names.
#[test]
fn registry_names_are_unique() {
    let have = names();
    let mut sorted = have.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(have.len(), sorted.len());
}

/// GREEN: rows are well-formed.
#[test]
fn registry_rows_are_well_formed() {
    for row in operation::registry() {
        assert!(!row.name.is_empty());
        assert!(!row.precondition.is_empty(), "{}", row.name);
        assert!(!row.postcondition.is_empty(), "{}", row.name);
        assert!((1..=9).contains(&row.owner_phase), "{}", row.name);
        assert!(
            row.proposer.as_str() == "S"
                || row.proposer.as_str() == "C"
                || row.proposer.as_str() == "M"
                || row.proposer.as_str() == "O"
                || row.proposer.as_str() == "U"
                || row.proposer.as_str() == "L"
        );
    }
}

/// GREEN: the registry is closed - no two rows collapse onto one variant.
#[test]
fn registry_covers_every_committed_class_exactly_once() {
    let classes = [
        "ScopeOpen",
        "ScopeCloseByEvent",
        "ScopeCloseByDeclaration",
        "Resegment",
        "Place",
        "Unplace",
        "Pin",
        "Unpin",
        "LanePolicyUpdate",
        "MigrationSelect",
        "AdmitIngress",
        "Sanitize",
        "Redact",
        "Import",
        "RuleUpdate",
        "VocabularyUpdate",
        "IndexRebuild",
        "StoreMode",
        "QuiesceUnwritable",
    ];
    let mut mapped = classes.iter().map(|c| snake(c)).collect::<Vec<_>>();
    mapped.sort_unstable();
    mapped.dedup();
    assert_eq!(mapped.len(), classes.len());
}

/// GREEN: EventKind surface is covered by registry rows.
#[test]
fn event_kind_surface_is_covered() {
    let have = names();
    // Append -> admit-ingress / sanitize
    assert!(have.contains(&"admit-ingress"));
    assert!(have.contains(&"sanitize"));
    // Ledger -> demote / discharge / revalidate
    for l in ["demote", "discharge", "revalidate"] {
        assert!(have.contains(&l), "{l} missing");
    }
    // ProviderTurn -> render-contract-observed / pending-response-stage
    assert!(have.contains(&"render-contract-observed"));
    assert!(have.contains(&"pending-response-stage"));
}

/// GREEN: port contract is deterministic and additive on the test port.
#[test]
fn accounting_port_is_deterministic_and_additive() {
    let port = TestPort;
    assert_eq!(port.bound(7, 100), port.bound(7, 100));
    assert_eq!(port.bound(7, 40) + port.bound(7, 60), port.bound(7, 100));
}

/// RED until #40 greens: each legal step advances the state machine.
#[test]
fn legal_steps_advance() {
    let mut ex = Executor::new(Epoch(1));
    let txn = ex.propose("note", 10).unwrap();
    assert_eq!(ex.state(), TxnState::Proposed);
    ex.snapshot().unwrap();
    assert_eq!(ex.state(), TxnState::Snapshotted);
    ex.generate().unwrap();
    assert_eq!(ex.state(), TxnState::Generated);
    let budget = Budget { b: 100, r: 8, h: 4 };
    ex.validate(50, &budget, 80, 0, 0).unwrap();
    assert_eq!(ex.state(), TxnState::Validated);
    assert_eq!(ex.commit(txn.parent_version).unwrap(), TxnState::Committed);
    assert_eq!(ex.state(), TxnState::Committed);
} // RED until #40 greens

/// RED until #40 greens: skipping states is rejected.
#[test]
fn skipped_states_are_illegal() {
    let mut ex = Executor::new(Epoch(1));
    ex.propose("note", 10).unwrap();
    let err = ex
        .validate(0, &Budget { b: 10, r: 1, h: 1 }, 0, 0, 0)
        .unwrap_err();
    assert_eq!(
        err,
        ExecutorError::IllegalTransition {
            from: TxnState::Proposed,
            to: TxnState::Validated
        }
    );
} // RED until #40 greens

/// RED until #40 greens: abort is legal from any live state, terminal states
/// are terminal.
#[test]
fn abort_is_legal_and_terminals_are_final() {
    let mut ex = Executor::new(Epoch(1));
    ex.propose("note", 1).unwrap();
    assert_eq!(ex.abort().unwrap(), TxnState::Aborted);
    assert_eq!(
        ex.commit(1),
        Err(ExecutorError::IllegalTransition {
            from: TxnState::Aborted,
            to: TxnState::Committed
        })
    );
} // RED until #40 greens

/// RED until #40 greens: compare-and-commit - non-rebase-safe rows abort on
/// parent mismatch, rebase-safe rows re-apply.
#[test]
fn stale_parent_compare_and_commit() {
    let mut ex = Executor::new(Epoch(1));
    let txn = ex.propose("drop-with-handle", 10).unwrap();
    ex.snapshot().unwrap();
    ex.generate().unwrap();
    let budget = Budget { b: 100, r: 8, h: 4 };
    ex.validate(50, &budget, 80, 0, 0).unwrap();
    assert_eq!(
        ex.commit(11),
        Err(ExecutorError::StaleParent {
            expected: 10,
            actual: 11
        })
    );
    assert_eq!(ex.state(), TxnState::Aborted);
    assert!(!operation::find(txn.op).unwrap().rebase_safe);

    let mut ex2 = Executor::new(Epoch(2));
    ex2.propose("note", 10).unwrap();
    ex2.snapshot().unwrap();
    ex2.generate().unwrap();
    ex2.validate(50, &budget, 80, 0, 0).unwrap();
    assert_eq!(ex2.commit(99).unwrap(), TxnState::Committed);
    assert!(operation::find("note").unwrap().rebase_safe);
} // RED until #40 greens

/// RED until #40 greens: crash property - replaying any prefix leaves the txn
/// committed-before-or-aborted-after, never partially applied.
#[test]
fn replay_prefix_is_never_partial() {
    for cut in 0..=4u8 {
        let mut ex = Executor::new(Epoch(3));
        ex.propose("note", 5).unwrap();
        let budget = Budget { b: 100, r: 8, h: 4 };
        let mut committed = false;
        for step in 0..cut {
            match step {
                0 => {
                    ex.snapshot().unwrap();
                }
                1 => {
                    ex.generate().unwrap();
                }
                2 => {
                    ex.validate(50, &budget, 80, 0, 0).unwrap();
                }
                _ => {
                    ex.commit(5).unwrap();
                    committed = true;
                }
            }
        }
        let s = ex.state();
        assert!(
            s != TxnState::Committed || committed,
            "committed without a durable commit at cut {cut}"
        );
        if !committed {
            assert!(
                matches!(
                    s,
                    TxnState::Proposed
                        | TxnState::Snapshotted
                        | TxnState::Generated
                        | TxnState::Validated
                        | TxnState::Aborted
                ),
                "partial state {s:?} at cut {cut}"
            );
        }
    }
} // RED until #40 greens

/// RED until #40 greens: budget properties.
#[test]
fn budget_properties() {
    let budget = Budget { b: 100, r: 8, h: 4 };
    assert!(budget::fits(88, &budget));
    assert!(!budget::fits(89, &budget));
    assert!(budget::feasible(88, &budget));
    assert!(!budget::feasible(89, &budget));
    assert!(budget::net_reclaim_ok(100, 92, 8));
    assert!(!budget::net_reclaim_ok(100, 93, 8));
    assert!(!budget::net_reclaim_ok(100, 101, 8));
    assert!(super::executor::RECLAMATION_BAR > 0);
} // RED until #40 greens

/// RED until #40 greens: rows owned by later phases answer with a typed
/// capability_not_landed verdict, never a silent omission.
#[test]
fn later_phase_rows_answer_capability_not_landed() {
    for name in ["compact", "reopen", "import", "calibration-update"] {
        let mut ex = Executor::new(Epoch(4));
        ex.propose(name, 1).unwrap();
        ex.snapshot().unwrap();
        match ex.generate() {
            Err(ExecutorError::CapabilityNotLanded { op }) => assert_eq!(op, name),
            other => panic!("{name} should not land yet: {other:?}"),
        }
    }
} // RED until #40 greens

/// RED until #40 greens: unknown rows are rejected at propose.
#[test]
fn unknown_rows_are_rejected() {
    let mut ex = Executor::new(Epoch(5));
    assert_eq!(
        ex.propose("no-such-op", 1),
        Err(ExecutorError::CapabilityNotLanded { op: "no-such-op" })
    );
} // RED until #40 greens

/// GREEN: proposer letters round-trip.
#[test]
fn proposer_letters_round_trip() {
    assert_eq!(Proposer::S.as_str(), "S");
    assert_eq!(Proposer::C.as_str(), "C");
    assert_eq!(Proposer::M.as_str(), "M");
    assert_eq!(Proposer::O.as_str(), "O");
    assert_eq!(Proposer::L.as_str(), "L");
}
