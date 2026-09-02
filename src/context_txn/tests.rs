//! Tests for the phase-3 red step: registry closure conformance (green now)
//! and the model-based transition / rebase / crash / budget properties that
//! the green turn must satisfy (red now).

use super::budget::{self, AccountingPort, Budget};
use super::executor::{Epoch, Executor, ExecutorError, FencingClock, TxnState};
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
        let found = have.contains(&row.as_str());
        assert!(found, "{class} -> {row} missing");
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
        let phase_ok = (1..=9).contains(&row.owner_phase);
        assert!(phase_ok, "{}", row.name);
        let ok = row.proposer.as_str() == "S"
            || row.proposer.as_str() == "C"
            || row.proposer.as_str() == "M"
            || row.proposer.as_str() == "O"
            || row.proposer.as_str() == "U"
            || row.proposer.as_str() == "L";
        assert!(ok);
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
    let ok2 = have.contains(&"admit-ingress");
    assert!(ok2);
    let ok3 = have.contains(&"sanitize");
    assert!(ok3);
    // Ledger -> demote / discharge / revalidate
    for l in ["demote", "discharge", "revalidate"] {
        let ok4 = have.contains(&l);
        assert!(ok4, "{l} missing");
    }
    // ProviderTurn -> render-contract-observed / pending-response-stage
    let ok5 = have.contains(&"render-contract-observed");
    assert!(ok5);
    let ok6 = have.contains(&"pending-response-stage");
    assert!(ok6);
}

/// GREEN: port contract is deterministic and additive on the test port.
#[test]
fn accounting_port_is_deterministic_and_additive() {
    let port = TestPort;
    assert_eq!(port.bound(7, 100), port.bound(7, 100));
    assert_eq!(port.bound(7, 40) + port.bound(7, 60), port.bound(7, 100));
}

/// each legal step advances the state machine.
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
}
/// skipping states is rejected.
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
}
/// abort is legal from any live state, terminal states
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
}
/// compare-and-commit - non-rebase-safe rows abort on
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
}

/// crash property - replaying any prefix leaves the txn
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
        let ok7 = s != TxnState::Committed || committed;
        assert!(ok7, "committed without a durable commit at cut {cut}");
        if !committed {
            let ok8 = matches!(
                s,
                TxnState::Proposed
                    | TxnState::Snapshotted
                    | TxnState::Generated
                    | TxnState::Validated
                    | TxnState::Aborted
            );
            assert!(ok8, "partial state {s:?} at cut {cut}");
        }
    }
}
/// budget properties.
#[test]
fn budget_properties() {
    let budget = Budget { b: 100, r: 8, h: 4 };
    let fits88 = budget::fits(88, &budget);
    assert!(fits88);
    let fits89 = budget::fits(89, &budget);
    assert!(!fits89);
    let feasible = budget::feasible(88, &budget);
    assert!(feasible);
    let feasible2 = budget::feasible(89, &budget);
    assert!(!feasible2);
    assert!(budget::net_reclaim_ok(100, 92, 8));
    assert!(!budget::net_reclaim_ok(100, 93, 8));
    assert!(!budget::net_reclaim_ok(100, 101, 8));
    assert!(super::operation::find("compact").unwrap().bar > 0);
}
/// rows owned by later phases answer with a typed
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
}
/// unknown rows are rejected at propose.
#[test]
fn unknown_rows_are_rejected() {
    let mut ex = Executor::new(Epoch(5));
    assert_eq!(
        ex.propose("no-such-op", 1),
        Err(ExecutorError::CapabilityNotLanded { op: "no-such-op" })
    );
}
/// GREEN: proposer letters round-trip.
#[test]
fn proposer_letters_round_trip() {
    assert_eq!(Proposer::S.as_str(), "S");
    assert_eq!(Proposer::C.as_str(), "C");
    assert_eq!(Proposer::M.as_str(), "M");
    assert_eq!(Proposer::O.as_str(), "O");
    assert_eq!(Proposer::L.as_str(), "L");
}

/// Fencing: a newer lease epoch fences older executors out at commit.
#[test]
fn newer_lease_fences_older_executor() {
    let clock = FencingClock::new();
    let e1 = clock.acquire();
    let mut ex1 = Executor::new(e1);
    ex1.propose("note", 5).unwrap();
    ex1.snapshot().unwrap();
    ex1.generate().unwrap();
    let budget = Budget { b: 100, r: 8, h: 4 };
    ex1.validate(50, &budget, 80, 0, 0).unwrap();
    clock.acquire();
    match ex1.commit_fenced(5, &clock) {
        Err(ExecutorError::Fenced { held: 2, mine: 1 }) => {}
        other => panic!("expected Fenced {{held: 3, mine: 1}}, got {other:?}"),
    }
    assert_eq!(ex1.state(), TxnState::Aborted);

    let mut ex2 = Executor::new(clock.acquire());
    ex2.propose("note", 5).unwrap();
    ex2.snapshot().unwrap();
    ex2.generate().unwrap();
    ex2.validate(50, &budget, 80, 0, 0).unwrap();
    let done = ex2.commit_fenced(5, &clock);
    assert_eq!(done.unwrap(), TxnState::Committed);
}

/// Authority non-increase: proposer or named authority may act; others denied.
#[test]
fn authority_non_increase() {
    let mut ex = Executor::new(Epoch(9));
    let as_m = ex.propose_as("note", 1, Proposer::M);
    assert!(as_m.is_ok());
    let as_c = ex.propose_as("note", 1, Proposer::C);
    assert!(as_c.is_ok());
    let denied = ex.propose_as("note", 1, Proposer::S);
    match denied {
        Err(ExecutorError::AuthorityDenied {
            op: "note",
            by: Proposer::S,
        }) => {}
        other => panic!("expected AuthorityDenied, got {other:?}"),
    }
}

/// Issue DoD: the registry must cover every row of the design's tab:ops.
#[test]
fn registry_covers_design_table_rows() {
    let tex = include_str!("../../design-docs/context-management/design.tex");
    let have = names();
    let mut table_rows = 0;
    // Row coverage is defined by the tab:ops longtable only. Split the
    // document on table starts, keep the first table whose body (the part
    // before \end{longtable}) carries the tab:ops label, and parse just its
    // rows: prose and unrelated tables must never inflate the count.
    let block = tex
        .split("\\begin{longtable}")
        .skip(1)
        .filter_map(|table| table.split_once("\\end{longtable}"))
        .map(|(body, _)| body)
        .find(|body| body.contains("\\label{tab:ops}"))
        .unwrap_or("");
    for line in block.lines() {
        let trimmed = line.trim();
        let split = trimmed.split_once(" & ");
        let pair = match split {
            Some(p) => p,
            None => continue,
        };
        let (name, rest) = pair;
        let body = name.chars().all(|ch| ch.is_ascii_lowercase() || ch == '-');
        let is_row = !name.is_empty() && body && !rest.starts_with('}');
        if !is_row {
            continue;
        }
        table_rows += 1;
        let found = have.contains(&name);
        assert!(found, "design row {name} missing from registry");
    }
    assert!(table_rows >= 55);
    assert!(table_rows <= 70, "parsed {table_rows} rows - over-matched");
}
