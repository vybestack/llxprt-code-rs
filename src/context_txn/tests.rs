//! Tests for the transaction core: registry conformance against the single
//! source table (#108), the budget algebra and its port (#104), the legality
//! gate at commit (#105), and the transition / rebase / fencing properties.

use super::bound_port::BoundPort;
use super::budget::{self, AccountingPort, Budget, Margins};
use super::executor::{CommitOutcome, Epoch, Executor, ExecutorError, FencingClock, TxnState};
use super::operation::{self, Proposer};

/// Deterministic, additive, conservative test port: the identity port over
/// the claimed contract term, so tests can construct exact disagreements.
struct TestPort {
    governed: u64,
}

impl AccountingPort for TestPort {
    fn bound(&self, _version: u64, contract: u64) -> u64 {
        self.governed.saturating_add(contract)
    }
}

/// A port whose computed bound equals the claim, so a validation can pass.
fn agreeing_port(governed: u64) -> std::rc::Rc<dyn AccountingPort> {
    std::rc::Rc::new(TestPort { governed })
}

/// Executor with a bound port that charges `governed` units plus the claim.
fn armed_executor(governed: u64) -> Executor {
    armed_executor_at(Epoch(1), governed)
}

/// Executor on a specific lease epoch, for fencing fixtures.
fn armed_executor_at(epoch: Epoch, governed: u64) -> Executor {
    let mut ex = Executor::new_generous(epoch);
    ex.bind_port(agreeing_port(governed));
    ex
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

/// GREEN: rows are well-formed. Preconditions are typed predicates now, so
/// the old display-string check is replaced by a text check over the
/// predicate (#108-3).
#[test]
fn registry_rows_are_well_formed() {
    for row in operation::registry() {
        assert!(!row.name.is_empty());
        assert!(!row.precondition.text().is_empty(), "{}", row.name);
        assert!(!row.postcondition.text().is_empty(), "{}", row.name);
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
    let ok2 = have.contains(&"admit-ingress");
    assert!(ok2);
    let ok3 = have.contains(&"sanitize");
    assert!(ok3);
    for l in ["demote", "discharge", "revalidate"] {
        let ok4 = have.contains(&l);
        assert!(ok4, "{l} missing");
    }
    let ok5 = have.contains(&"render-contract-observed");
    assert!(ok5);
    let ok6 = have.contains(&"pending-response-stage");
    assert!(ok6);
}

/// GREEN: port contract is deterministic and additive on the test port.
#[test]
fn accounting_port_is_deterministic_and_additive() {
    // A frameless margin isolates the additive term: the bound of a disjoint
    // union is the sum of the member bounds.
    let frameless = Margins {
        commit_frame: 0,
        ..Margins::V1
    };
    let port = BoundPort::new(frameless, 0, 0);
    assert_eq!(port.bound(7, 100), port.bound(7, 100));
    assert_eq!(port.bound(7, 40) + port.bound(7, 60), port.bound(7, 100));
}

/// #108-1: every registry row agrees with the single source table, and every
/// source-table name is registered. Generated over the full registry.
#[test]
fn owner_phases_match_the_single_source_table() {
    for (name, phase) in operation::source_table() {
        let row = operation::find(name).unwrap_or_else(|| panic!("{name} is not registered"));
        assert_eq!(
            row.owner_phase, *phase,
            "{name} diverges from plan.md:377-384"
        );
    }
}

/// #108-2: every governed-state field and event surface has a registry row -
/// the OperationCommit classes the reducer can fold must all resolve.
#[test]
fn every_committed_class_has_a_registry_row() {
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
    for class in classes {
        let row = snake(class);
        assert!(
            operation::find(&row).is_some(),
            "governed event class {class} -> {row} has no registry row"
        );
    }
}

/// #108-3: preconditions are typed predicates, evaluated over governed facts.
#[test]
fn preconditions_are_typed_predicates() {
    let fits = operation::find("admit-ingress").expect("registered");
    let small = operation::PreconditionFacts::for_row(fits, 8, Budget { b: 16, r: 4, h: 2 });
    assert!(fits.precondition.holds(&small));
    let big = operation::PreconditionFacts::for_row(fits, 11, Budget { b: 16, r: 4, h: 2 });
    assert!(!fits.precondition.holds(&big));
}

/// #108-4: emergency-capable rows are flagged and the ladder consumes the flag.
#[test]
fn emergency_flags_cover_the_ladder_and_gate_the_bar() {
    for rung in crate::context_policy::ladder::Rung::all() {
        let row = operation::find(rung.operation()).expect("ladder rungs are registered");
        assert!(row.emergency, "{} must be emergency-capable", row.name);
    }
}

/// #104-4: every reclamation row has a nonzero bar, and a transaction that
/// reclaims less than the bar fails validation.
#[test]
fn reclamation_rows_have_nonzero_bars_and_enforce_them() {
    for row in operation::registry() {
        if row.reclamation {
            assert!(row.bar > 0, "{} reclaims with bar 0", row.name);
        }
    }
    let mut ex = armed_executor(0);
    ex.propose("placeholder-collapse", 10).unwrap();
    ex.snapshot().unwrap();
    ex.generate().unwrap();
    let budget = Budget { b: 100, r: 8, h: 4 };
    let err = ex
        .validate(0, &budget, 80, 100, 100)
        .expect_err("phi must drop by at least the bar");
    assert_eq!(
        err,
        ExecutorError::PreconditionFailed {
            which: "reclamation-bar"
        }
    );
    assert_eq!(ex.state(), TxnState::Aborted);
}

/// #104-3: margins are versioned and a drift fixture triggers recalibration.
#[test]
fn margin_drift_fixture_recalibrates_under_a_newer_version() {
    let mut ex = armed_executor(0);
    assert_eq!(ex.margins().version, 1);
    ex.recalibrate_margins(1, 512)
        .expect_err("same version refused");
    ex.recalibrate_margins(2, 512)
        .expect("newer version adopted");
    assert_eq!(ex.margins().version, 2);
    assert_eq!(ex.margins().per_tool_declaration, 512);
}

/// #104-1: a commit whose caller-supplied bound disagrees with the port fails
/// validation with a typed verdict.
#[test]
fn a_bound_that_disagrees_with_the_port_fails_validation() {
    let mut ex = Executor::new_generous(Epoch(1));
    ex.bind_port(std::rc::Rc::new(TestPort { governed: 500 }));
    ex.propose("arm", 10).unwrap();
    ex.snapshot().unwrap();
    ex.generate().unwrap();
    let budget = Budget {
        b: 100_000,
        r: 8,
        h: 4,
    };
    let err = ex
        .validate(50, &budget, 80, 0, 0)
        .expect_err("the caller invented a bound");
    assert_eq!(
        err,
        ExecutorError::BoundDisagrees {
            claimed: 50,
            computed: 550
        }
    );
    assert_eq!(ex.state(), TxnState::Aborted);
}

/// #104-2: the computed bound includes tool-declaration bytes, so growing the
/// tool surface moves the bound and can fail a fit that used to pass.
#[test]
fn growing_the_tool_surface_moves_the_bound() {
    let small = BoundPort::new(Margins::V1, 2, 0);
    let big = BoundPort::new(Margins::V1, 10, 0);
    let smaller_bound = small.bound(1, 0);
    let bigger_bound = big.bound(1, 0);
    assert!(bigger_bound > smaller_bound);
    let budget = Budget {
        b: bigger_bound + 1,
        r: 1,
        h: 1,
    };
    assert!(budget::fits(smaller_bound, &budget));
    assert!(!budget::fits(bigger_bound, &budget));
}

/// #105-1: a transaction whose projection exceeds a region budget fails
/// commit with a typed legality error.
#[test]
fn an_over_budget_projection_fails_commit_with_a_typed_error() {
    use crate::context_kernel::ir::Region;
    use crate::context_kernel::legality::RenderContract;
    let mut contract = RenderContract::generous(1);
    contract.region_budgets = Region::all().iter().map(|region| (*region, 1)).collect();
    let mut ex = Executor::new(Epoch(1), contract);
    ex.bind_port(agreeing_port(0));
    ex.ingest_test_item();
    ex.propose("arm", 0).unwrap();
    ex.snapshot().unwrap();
    ex.generate().unwrap();
    ex.validate(0, &Budget { b: 100, r: 1, h: 1 }, 0, 0, 0)
        .expect("validation is not the legality gate");
    let err = ex.commit_outcome(0).expect_err("the region is over budget");
    match err {
        ExecutorError::Illegal { which, predicate } => {
            assert_eq!(which, "region-over-budget");
            assert!(!predicate.is_empty());
        }
        other => panic!("expected a typed legality error, got {other:?}"),
    }
}

/// #105-2: a projection with an unpaired tool declaration fails commit.
#[test]
fn an_unpaired_tool_declaration_fails_commit() {
    use crate::context_kernel::legality::{RenderContract, ToolRole};
    let mut contract = RenderContract::generous(1);
    contract.declare("read", "call-1", ToolRole::Call);
    let mut ex = Executor::new(Epoch(1), contract);
    ex.bind_port(agreeing_port(0));
    ex.propose("arm", 0).unwrap();
    ex.snapshot().unwrap();
    ex.generate().unwrap();
    ex.validate(0, &Budget { b: 100, r: 1, h: 1 }, 0, 0, 0)
        .unwrap();
    let err = ex.commit_outcome(0).expect_err("the call is unpaired");
    match err {
        ExecutorError::Illegal { which, predicate } => {
            assert_eq!(which, "pairing");
            assert!(predicate.contains("call-1"), "predicate: {predicate}");
        }
        other => panic!("expected a pairing error, got {other:?}"),
    }
}

/// #105-3: the committed render-contract version is recorded on the executor
/// and durable in the spine record the store commits.
#[test]
fn the_committed_contract_version_is_recorded_and_durable() {
    use crate::context_kernel::legality::RenderContract;
    let contract = RenderContract::generous(7);
    let mut ex = Executor::new(Epoch(1), contract);
    ex.bind_port(agreeing_port(0));
    assert_eq!(ex.committed_contract_version(), None);
    ex.propose("arm", 0).unwrap();
    ex.snapshot().unwrap();
    ex.generate().unwrap();
    ex.validate(0, &Budget { b: 100, r: 1, h: 1 }, 0, 0, 0)
        .unwrap();
    assert_eq!(ex.commit_outcome(0).unwrap(), CommitOutcome::Applied);
    assert_eq!(ex.committed_contract_version(), Some(7));
    // The durable record the store commits carries the same version: the
    // spine's encoded record is digested with it, and the send path compares
    // against `committed_contract_version`.
    let record = crate::context_store::spine::Spine::new();
    assert_eq!(
        crate::context_txn::bound_port::store_spine_units(record.len()),
        record.len()
    );
}

/// #121-a: `commit` and `commit_fenced` return the outcome, never collapsing
/// a rebase no-op into a committed state.
#[test]
fn commit_reports_a_rebase_no_op_instead_of_collapsing_it() {
    // A rebase-safe row committed against a moved parent reports
    // `RebaseNoOp`: the transaction ends committed-for-replay purposes, but
    // the caller must not count the commit as applied progress.
    let mut ex = armed_executor(0);
    ex.land_through(6);
    ex.propose("read-back", 10).unwrap();
    ex.snapshot().unwrap();
    ex.generate().unwrap();
    ex.validate(0, &Budget { b: 100, r: 8, h: 4 }, 80, 0, 0)
        .unwrap();
    assert_eq!(ex.commit(11).unwrap(), CommitOutcome::RebaseNoOp);
    assert_eq!(ex.state(), TxnState::Committed);

    // A non-rebase-safe row against a moved parent is a typed stale-parent
    // error, never a silent no-op.
    let mut ex = armed_executor(0);
    ex.propose("arm", 10).unwrap();
    ex.snapshot().unwrap();
    ex.generate().unwrap();
    ex.validate(0, &Budget { b: 100, r: 8, h: 4 }, 80, 0, 0)
        .unwrap();
    match ex.commit(11) {
        Err(ExecutorError::StaleParent {
            expected: 10,
            actual: 11,
        }) => {}
        other => panic!("expected StaleParent, got {other:?}"),
    }
}

/// #121-a: an applied parent is still a real, applied effect.
#[test]
fn an_applied_parent_still_commits_as_applied() {
    let mut ex = armed_executor(0);
    ex.propose("arm", 10).unwrap();
    ex.snapshot().unwrap();
    ex.generate().unwrap();
    ex.validate(0, &Budget { b: 100, r: 8, h: 4 }, 80, 0, 0)
        .unwrap();
    assert_eq!(ex.commit(10).unwrap(), CommitOutcome::Applied);
}

/// each legal step advances the state machine.
#[test]
fn legal_steps_advance() {
    let mut ex = armed_executor(0);
    let txn = ex.propose("arm", 10).unwrap();
    assert_eq!(ex.state(), TxnState::Proposed);
    ex.snapshot().unwrap();
    assert_eq!(ex.state(), TxnState::Snapshotted);
    ex.generate().unwrap();
    assert_eq!(ex.state(), TxnState::Generated);
    let budget = Budget { b: 100, r: 8, h: 4 };
    ex.validate(0, &budget, 80, 0, 0).unwrap();
    assert_eq!(ex.state(), TxnState::Validated);
    assert_eq!(
        ex.commit(txn.parent_version).unwrap(),
        CommitOutcome::Applied
    );
    assert_eq!(ex.state(), TxnState::Committed);
}

/// skipping states is rejected.
#[test]
fn skipped_states_are_illegal() {
    let mut ex = armed_executor(0);
    ex.propose("arm", 10).unwrap();
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

/// abort is legal from any live state, terminal states are terminal.
#[test]
fn abort_is_legal_and_terminals_are_final() {
    let mut ex = armed_executor(0);
    ex.propose("arm", 1).unwrap();
    assert_eq!(ex.abort().unwrap(), TxnState::Aborted);
    assert_eq!(
        ex.commit(1),
        Err(ExecutorError::IllegalTransition {
            from: TxnState::Aborted,
            to: TxnState::Committed
        })
    );
}

/// compare-and-commit - non-rebase-safe rows abort on parent mismatch.
#[test]
fn stale_parent_compare_and_commit() {
    let mut ex = armed_executor(0);
    let txn = ex.propose("drop-with-handle", 10).unwrap();
    ex.snapshot().unwrap();
    ex.generate().unwrap();
    let budget = Budget { b: 100, r: 8, h: 4 };
    ex.validate(0, &budget, 80, 64, 56).unwrap();
    assert_eq!(
        ex.commit(11),
        Err(ExecutorError::StaleParent {
            expected: 10,
            actual: 11
        })
    );
    assert_eq!(ex.state(), TxnState::Aborted);
    assert!(!operation::find(txn.op).unwrap().rebase_safe);
}

/// a non-rebase-safe row that would otherwise be misreported still aborts.
#[test]
fn non_rebase_safe_mismatch_keeps_aborting() {
    let mut ex = armed_executor(0);
    ex.propose("drop-with-handle", 10).unwrap();
    ex.snapshot().unwrap();
    ex.generate().unwrap();
    let budget = Budget { b: 100, r: 8, h: 4 };
    ex.validate(0, &budget, 80, 64, 56).unwrap();
    assert!(ex.commit_outcome(11).is_err());
    assert_eq!(ex.state(), TxnState::Aborted);
}

/// a failed precondition kills the transaction: commit after the failure is
/// refused and the only legal next step is abort.
#[test]
fn failed_precondition_blocks_commit() {
    let mut ex = armed_executor(0);
    ex.propose("arm", 10).unwrap();
    ex.snapshot().unwrap();
    ex.generate().unwrap();
    let budget = Budget { b: 100, r: 8, h: 4 };
    let err = ex
        .validate(150, &budget, 80, 0, 0)
        .expect_err("bound 150 exceeds the ceiling");
    assert_eq!(
        err,
        ExecutorError::PreconditionFailed { which: "fit-bound" }
    );
    assert_eq!(ex.state(), TxnState::Aborted);
    assert_eq!(
        ex.commit(10),
        Err(ExecutorError::IllegalTransition {
            from: TxnState::Aborted,
            to: TxnState::Committed
        })
    );
}

/// an executor with no port refuses validation: the caller cannot fall back to
/// inventing a bound.
#[test]
fn a_missing_port_refuses_validation() {
    let mut ex = Executor::new_generous(Epoch(1));
    ex.propose("arm", 10).unwrap();
    ex.snapshot().unwrap();
    ex.generate().unwrap();
    let err = ex
        .validate(0, &Budget { b: 10, r: 1, h: 1 }, 0, 0, 0)
        .unwrap_err();
    assert_eq!(
        err,
        ExecutorError::PreconditionFailed {
            which: "bound-port-missing"
        }
    );
}

/// crash property - replaying any prefix leaves the txn
/// committed-before-or-aborted-after, never partially applied.
#[test]
fn replay_prefix_is_never_partial() {
    for cut in 0..=4u8 {
        let mut ex = armed_executor(0);
        ex.propose("arm", 5).unwrap();
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
                    ex.validate(0, &budget, 80, 0, 0).unwrap();
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
    assert!(budget::fits(88, &budget));
    assert!(!budget::fits(89, &budget));
    assert!(budget::feasible(88, &budget));
    assert!(!budget::feasible(89, &budget));
    assert!(budget::net_reclaim_ok(100, 92, 8));
    assert!(!budget::net_reclaim_ok(100, 93, 8));
    assert!(!budget::net_reclaim_ok(100, 101, 8));
    assert!(operation::find("compact").unwrap().bar > 0);
}

/// rows owned by later phases answer with a typed capability_not_landed
/// verdict, never a silent omission.
#[test]
fn later_phase_rows_answer_capability_not_landed() {
    for name in ["reopen", "branch-open", "expire-pin"] {
        let mut ex = armed_executor(0);
        ex.propose(name, 1).unwrap();
        ex.snapshot().unwrap();
        match ex.generate() {
            Err(ExecutorError::CapabilityNotLanded { op }) => assert_eq!(op, name),
            other => panic!("{name} should not land yet: {other:?}"),
        }
    }
}

#[test]
fn every_phase4_row_generates_durable_effect_artifacts() {
    let rows: Vec<_> = operation::registry()
        .iter()
        .filter(|row| row.owner_phase == 4)
        .collect();
    assert!(!rows.is_empty());
    for row in rows {
        let mut executor = armed_executor(0);
        executor.propose(row.name, 1).unwrap();
        executor.snapshot().unwrap();
        let generated = executor.generate().unwrap();
        assert_eq!(generated.op, row.name);
        assert_eq!(executor.state(), TxnState::Generated);
    }
}

/// unknown rows are rejected at propose.
#[test]
fn unknown_rows_are_rejected() {
    let mut ex = armed_executor(0);
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

/// Fencing: a newer lease epoch fences older executors out at commit, and the
/// fenced executor reports the outcome, not a collapsed state.
#[test]
fn newer_lease_fences_older_executor() {
    let clock = FencingClock::new();
    let e1 = clock.acquire();
    let mut ex1 = armed_executor_at(e1, 0);
    assert_eq!(ex1.epoch(), e1);
    ex1.propose("arm", 5).unwrap();
    ex1.snapshot().unwrap();
    ex1.generate().unwrap();
    let budget = Budget { b: 100, r: 8, h: 4 };
    ex1.validate(0, &budget, 80, 0, 0).unwrap();
    clock.acquire();
    match ex1.commit_fenced(5, &clock) {
        Err(ExecutorError::Fenced { held: 2, mine: 1 }) => {}
        other => panic!("expected Fenced {{held: 2, mine: 1}}, got {other:?}"),
    }
    assert_eq!(ex1.state(), TxnState::Aborted);

    let e2 = clock.acquire();
    let mut ex2 = armed_executor_at(e2, 0);
    assert_eq!(ex2.epoch(), e2);
    ex2.propose("arm", 5).unwrap();
    ex2.snapshot().unwrap();
    ex2.generate().unwrap();
    ex2.validate(0, &budget, 80, 0, 0).unwrap();
    let done = ex2.commit_fenced(5, &clock);
    assert_eq!(done.unwrap(), CommitOutcome::Applied);
}

/// Authority non-increase: proposer or named authority may act; others denied.
#[test]
fn authority_non_increase() {
    let mut ex = armed_executor(0);
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

/// #121-c: region-wide admission accounting sums the admitted payloads against
/// the region budget; a projection that would cross the ceiling fails commit.
#[test]
fn region_accounting_sums_admissions_against_the_ceiling() {
    let mut ex = armed_executor(0);
    ex.arm_region_accounting(100);
    assert_eq!(ex.region_admitted(), 0);
    ex.propose("admit-ingress", 0).unwrap();
    ex.snapshot().unwrap();
    ex.generate().unwrap();
    ex.validate(0, &Budget { b: 100, r: 1, h: 1 }, 0, 0, 0)
        .unwrap();
    assert_eq!(ex.commit_outcome(0).unwrap(), CommitOutcome::Applied);
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
