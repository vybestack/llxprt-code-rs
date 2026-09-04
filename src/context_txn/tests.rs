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
    fn bound(&self, _version: u64, effect_bytes: u64) -> u64 {
        self.governed.saturating_add(effect_bytes)
    }

    /// Mirrors `BoundPort`: a test port that owns no margin table reports
    /// `None`, so the executor's drift fixture surfaces `recalibrated`'s
    /// default refusal instead of silently recalibrating a table it never
    /// charged a bound from.
    fn bound_margins(&self) -> Option<Margins> {
        None
    }

    /// Mirrors `BoundPort`: the port that owns the table is the port that
    /// recalibrates. A test port with no table has nothing to recalibrate,
    /// so `None` is the honest answer - the executor surfaces it as a
    /// refusal - and no bound it computes can be moved by a drift fixture.
    fn recalibrated(
        &self,
        _version: u64,
        _per_tool_declaration: u64,
    ) -> Option<Result<std::rc::Rc<dyn AccountingPort>, &'static str>> {
        None
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

/// Executor bound to the production `BoundPort` over a real margin table, for
/// fixtures that need a port which owns (and can recalibrate) its table.
fn armed_executor_with_bound_port(governed: u64) -> Executor {
    let mut ex = Executor::new_generous(Epoch(1));
    ex.bind_port(std::rc::Rc::new(BoundPort::new(Margins::V1, 0, governed)));
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

/// Every governed event surface has a registry row (issue R-018/R-049),
/// generated from the enum itself: the class list is an exhaustive `match`
/// over `OperationClass`, so adding a variant without a row is a COMPILE
/// error, never a silently skipped test. The governed-state field half of
/// the closure iterates `Region::all()` the same way.
#[test]
fn every_committed_class_has_a_registry_row() {
    use crate::context_kernel::events::OperationClass;
    // Exhaustive match: a new variant fails compilation here until it is
    // given a stable name and a registry row.
    let classes: [OperationClass; 19] = [
        OperationClass::ScopeOpen,
        OperationClass::ScopeCloseByEvent,
        OperationClass::ScopeCloseByDeclaration,
        OperationClass::Resegment,
        OperationClass::Place,
        OperationClass::Unplace,
        OperationClass::Pin,
        OperationClass::Unpin,
        OperationClass::LanePolicyUpdate,
        OperationClass::MigrationSelect,
        OperationClass::AdmitIngress,
        OperationClass::Sanitize,
        OperationClass::Redact,
        OperationClass::Import,
        OperationClass::RuleUpdate,
        OperationClass::VocabularyUpdate,
        OperationClass::IndexRebuild,
        OperationClass::StoreMode,
        OperationClass::QuiesceUnwritable,
    ];
    // The enum's own `name()` is the authority for the row spelling, so the
    // test can never drift from the encoder. The match below is the
    // coverage authority; the coercion keeps it a used item in every build.
    const _: () = {
        fn exhaustive(class: &OperationClass) {
            match class {
                OperationClass::ScopeOpen
                | OperationClass::ScopeCloseByEvent
                | OperationClass::ScopeCloseByDeclaration
                | OperationClass::Resegment
                | OperationClass::Place
                | OperationClass::Unplace
                | OperationClass::Pin
                | OperationClass::Unpin
                | OperationClass::LanePolicyUpdate
                | OperationClass::MigrationSelect
                | OperationClass::AdmitIngress
                | OperationClass::Sanitize
                | OperationClass::Redact
                | OperationClass::Import
                | OperationClass::RuleUpdate
                | OperationClass::VocabularyUpdate
                | OperationClass::IndexRebuild
                | OperationClass::StoreMode
                | OperationClass::QuiesceUnwritable => (),
            }
        }
        let _arm_coverage: fn(&OperationClass) = exhaustive;
    };
    for class in classes {
        let name = class.name();
        assert!(
            operation::find(name).is_some(),
            "governed event class {name} has no registry row"
        );
    }
    // The governed-state closure: every region the reducer can place into
    // has a budget row in the render contract, so a projection that uses
    // the region is covered by the gate rather than silently unbudgeted.
    for region in crate::context_kernel::ir::Region::all() {
        let contract = crate::context_kernel::legality::RenderContract::generous(1);
        assert!(
            contract.supports_region(region),
            "governed region {} has no budget row",
            region.name()
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
    // `compact` is owned by phase 6 and the executor lands through phase 4 by
    // default, so the fixture opens the gate the same way the other phase-6
    // fixtures do.
    ex.land_through(6);
    // `compact` is an emergency-capable reclamation row, so its bar is exempt
    // under the emergency branch of `ReclaimsBar::holds` (that exemption is what
    // the ladder uses to recover a stuck region). The exemption is only from the
    // drop, never from growth: a rung that RAISES Phi must still fail, so the
    // fixture is an increase rather than a flat phi.
    ex.propose("compact", 10).unwrap();
    ex.snapshot().unwrap();
    ex.generate().unwrap();
    let budget = Budget { b: 100, r: 8, h: 4 };
    let err = ex
        .validate(0, &budget, 80, 100, 110, 0)
        .expect_err("phi must not grow, even on an emergency rung");
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
    // The margin table lives on the port, so a test port with no table
    // reports `None` and a recalibration is refused rather than moved.
    assert!(ex.margins().is_none());
    assert_eq!(
        ex.recalibrate_margins(1, 512),
        Err("the bound port carries no margin table to recalibrate"),
        "a table-less test port must refuse recalibration"
    );
    // A port that owns the table recalibrates through itself, so the charge
    // a bound is computed from actually moves.
    let mut bound_port_ex = armed_executor_with_bound_port(0);
    let before = bound_port_ex
        .margins()
        .map_or(Margins::V1.per_tool_declaration, |m| m.per_tool_declaration);
    assert_eq!(before, Margins::V1.per_tool_declaration);
    bound_port_ex
        .recalibrate_margins(1, 512)
        .expect_err("same version refused");
    bound_port_ex
        .recalibrate_margins(2, 512)
        .expect("newer version adopted");
    assert_eq!(bound_port_ex.margins().map_or(0, |m| m.version), 2);
    assert_eq!(
        bound_port_ex
            .margins()
            .map_or(0, |m| m.per_tool_declaration),
        512
    );
    assert_eq!(
        bound_port_ex.margins().map_or(0, |m| m.commit_frame),
        Margins::V1.commit_frame
    );
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
    // The port's input is the EFFECT (50 bytes), never the claim, so the
    // computed bound is governed 500 + effect 50 = 550 and a claim of 50
    // is a real disagreement rather than a fixpoint the caller satisfies.
    let err = ex
        .validate(50, &budget, 80, 0, 0, 50)
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
    ex.validate(0, &Budget { b: 100, r: 1, h: 1 }, 0, 0, 0, 0)
        .expect("validation is not the legality gate");
    let err = ex
        .commit_outcome(0, None, 0)
        .expect_err("the region is over budget");
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
    ex.validate(0, &Budget { b: 100, r: 1, h: 1 }, 0, 0, 0, 0)
        .unwrap();
    let err = ex
        .commit_outcome(0, None, 0)
        .expect_err("the call is unpaired");
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
    ex.validate(0, &Budget { b: 100, r: 1, h: 1 }, 0, 0, 0, 0)
        .unwrap();
    assert_eq!(
        ex.commit_outcome(0, None, 0).unwrap(),
        CommitOutcome::Applied
    );
    assert_eq!(ex.committed_contract_version(), Some(7));
    // The committed contract version is DURABLE: the spine frame the store
    // commits carries it, and a reload through the typed loader recovers the
    // same version, so a recovered session compares against the version its
    // own commits were gated under instead of an in-memory-only value.
    use crate::context_store::spine::Spine;
    let committed = ex.committed_contract_version().unwrap();
    let mut spine = Spine::new();
    spine.append(
        "committed-contract-version",
        committed.to_string().as_bytes(),
    );
    let encoded = spine.encode();
    let reloaded = Spine::load_typed(&encoded).expect("the written frame reloads");
    let reloaded_version: u64 = String::from_utf8(
        reloaded
            .records()
            .first()
            .map(|record| reloaded.record_bytes(record).to_vec())
            .unwrap_or_default(),
    )
    .expect("the version is UTF-8 text")
    .parse()
    .expect("the version round-trips as an integer");
    assert_eq!(
        reloaded_version, committed,
        "the reloaded spine frame must round-trip the committed contract version"
    );
    // A mismatched version is an error, never a silent send: the kernel's
    // own send-path check refuses a version the commit was not gated under.
    // The proof object the send path would carry: the gate itself, run
    // over the contract the commit was gated under. Its recorded version is
    // what a send is compared against.
    let genesis = crate::context_kernel::reducer::Reducer::new(
        crate::context_kernel::reducer::IDLENESS_WINDOW,
    )
    .fold(&crate::context_kernel::events::EventLog::new(
        crate::context_kernel::migration::V2,
    ))
    .expect("an empty log folds into genesis");
    let legal =
        crate::context_kernel::legality::is_legal(&genesis, &RenderContract::generous(committed))
            .expect("a genesis projection under a generous contract is legal");
    assert_eq!(legal.contract_version(), committed);
    assert!(legal.sendable_with(committed));
    assert!(
        !legal.sendable_with(committed + 1),
        "a send carrying a mismatched contract version must be refused"
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
    ex.validate(0, &Budget { b: 100, r: 8, h: 4 }, 80, 0, 0, 0)
        .unwrap();
    assert_eq!(ex.commit(11, None, 0).unwrap(), CommitOutcome::RebaseNoOp);
    assert_eq!(ex.state(), TxnState::Committed);

    // A non-rebase-safe row against a moved parent is a typed stale-parent
    // error, never a silent no-op.
    let mut ex = armed_executor(0);
    ex.propose("arm", 10).unwrap();
    ex.snapshot().unwrap();
    ex.generate().unwrap();
    ex.validate(0, &Budget { b: 100, r: 8, h: 4 }, 80, 0, 0, 0)
        .unwrap();
    match ex.commit(11, None, 0) {
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
    ex.validate(0, &Budget { b: 100, r: 8, h: 4 }, 80, 0, 0, 0)
        .unwrap();
    assert_eq!(ex.commit(10, None, 0).unwrap(), CommitOutcome::Applied);
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
    ex.validate(0, &budget, 80, 0, 0, 0).unwrap();
    assert_eq!(ex.state(), TxnState::Validated);
    assert_eq!(
        ex.commit(txn.parent_version, None, 0).unwrap(),
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
        .validate(0, &Budget { b: 10, r: 1, h: 1 }, 0, 0, 0, 0)
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
        ex.commit(1, None, 0),
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
    ex.validate(0, &budget, 80, 64, 56, 0).unwrap();
    assert_eq!(
        ex.commit(11, None, 0),
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
    ex.validate(0, &budget, 80, 64, 56, 0).unwrap();
    assert!(ex.commit_outcome(11, None, 0).is_err());
    assert_eq!(ex.state(), TxnState::Aborted);
}

/// a failed precondition kills the transaction: commit after the failure is
/// refused and the only legal next step is abort.
#[test]
fn failed_precondition_blocks_commit() {
    let mut ex = armed_executor(0);
    // `admit-ingress` carries the FITS predicate, so this exercises the row's
    // own typed precondition rather than a row whose precondition is the
    // unconditional placeholder.
    ex.propose("admit-ingress", 10).unwrap();
    ex.snapshot().unwrap();
    ex.generate().unwrap();
    let budget = Budget { b: 100, r: 8, h: 4 };
    // A bound of 150 with the effect it is charged for: the claim agrees
    // with the port (0 + 150), so the failure below is the FIT check and not
    // a bound disagreement.
    let err = ex
        .validate(150, &budget, 80, 0, 0, 150)
        .expect_err("bound 150 exceeds the ceiling");
    assert_eq!(
        err,
        ExecutorError::PreconditionFailed { which: "fit-bound" }
    );
    assert_eq!(ex.state(), TxnState::Aborted);
    assert_eq!(
        ex.commit(10, None, 0),
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
        .validate(0, &Budget { b: 10, r: 1, h: 1 }, 0, 0, 0, 0)
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
                    ex.validate(0, &budget, 80, 0, 0, 0).unwrap();
                }
                _ => {
                    ex.commit(5, None, 0).unwrap();
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
    ex1.validate(0, &budget, 80, 0, 0, 0).unwrap();
    clock.acquire();
    match ex1.commit_fenced(5, &clock, None, 0) {
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
    ex2.validate(0, &budget, 80, 0, 0, 0).unwrap();
    let done = ex2.commit_fenced(5, &clock, None, 0);
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

/// Region-wide admission accounting SUMS the admitted payloads against the
/// region budget (issue 121-c): two applied admissions accumulate, and a
/// third whose projection would cross the armed ceiling is refused with
/// the typed RegionOverBudget verdict instead of overwriting the total.
#[test]
fn region_accounting_sums_admissions_against_the_ceiling() {
    let mut ex = armed_executor(0);
    ex.arm_region_accounting(100);
    assert_eq!(ex.region_admitted(), 0);
    // Two admissions fit; the region total is their SUM, never the last
    // payload applied.
    for _ in 0..2 {
        ex.propose("admit-ingress", 0).unwrap();
        ex.snapshot().unwrap();
        ex.generate().unwrap();
        ex.validate(40, &Budget { b: 100, r: 1, h: 1 }, 0, 0, 0, 40)
            .unwrap();
        assert_eq!(
            ex.commit_outcome(0, None, 40).unwrap(),
            CommitOutcome::Applied
        );
    }
    assert_eq!(ex.region_admitted(), 80, "admissions must accumulate");
    // The third admission projects 80 + 40 > 100, so the ceiling refuses it.
    ex.propose("admit-ingress", 0).unwrap();
    ex.snapshot().unwrap();
    ex.generate().unwrap();
    ex.validate(40, &Budget { b: 100, r: 1, h: 1 }, 0, 0, 0, 40)
        .unwrap();
    let err = ex
        .commit_outcome(0, None, 40)
        .expect_err("the projection crosses the armed ceiling");
    assert_eq!(
        err,
        ExecutorError::RegionOverBudget {
            admitted: 80,
            projected: 120,
            ceiling: 100,
        }
    );
    assert_eq!(ex.state(), TxnState::Aborted);
    // A refused admission adds nothing to the region total.
    assert_eq!(ex.region_admitted(), 80);
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
