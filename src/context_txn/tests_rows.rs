//! Registry-row coverage tests split from `tests.rs` to keep that file
//! inside the xtask quality effective-LOC ceiling.

use super::budget::Budget;
use super::executor::{Epoch, Executor, ExecutorError, TxnState};

/// An executor on the default lease epoch with an agreeing port, mirroring
/// the `armed_executor` fixture in `tests.rs`.
fn armed_executor(governed: u64) -> Executor {
    let mut ex = Executor::new_generous(Epoch(1));
    ex.bind_port(std::rc::Rc::new(super::bound_port::BoundPort::new(
        super::budget::Margins::V1,
        0,
        governed,
    )));
    ex
}

/// The row names the registry exposes, mirroring the helper in `tests.rs`.
fn names() -> Vec<&'static str> {
    super::operation::registry()
        .iter()
        .map(|r| r.name)
        .collect()
}

/// The closed operation vocabulary is a test contract, not a build dependency on
/// an internal proposal or TeX design. Exact equality also catches unexpected or
/// duplicate registrations; the former document parser checked only a subset.
#[test]
fn registry_matches_closed_operation_contract() {
    let expected = [
        "admit-as-handle",
        "admit-convenience",
        "admit-feedback",
        "admit-ingress",
        "admit-obligation",
        "admit-observation",
        "admit-response",
        "amend",
        "arm",
        "branch-abort",
        "branch-open",
        "branch-return",
        "calibration-update",
        "checkpoint",
        "compact",
        "condense",
        "consolidate-head",
        "consolidate-notes",
        "consolidate-obligations",
        "decay",
        "demote",
        "disarm",
        "discharge",
        "drop-with-handle",
        "escalate-complete",
        "escalate-pending",
        "expire-pin",
        "fact-invalidate",
        "flush-notes",
        "fold",
        "fold-away-ephemeral",
        "import",
        "index-rebuild",
        "journal-append",
        "lane-policy-update",
        "lease-acquire",
        "margin-recalibrate",
        "migration-select",
        "note",
        "pending-response-stage",
        "pin",
        "pin-override-collapse",
        "place",
        "placeholder-collapse",
        "promote",
        "queue-service",
        "quiesce-unwritable",
        "re-promote",
        "read-back",
        "reclassify",
        "redact",
        "regenerate",
        "render-contract-observed",
        "reopen",
        "repair-result",
        "resegment",
        "retract",
        "revalidate",
        "rule-update",
        "sanitize",
        "scope-close-by-declaration",
        "scope-close-by-event",
        "scope-open",
        "spend-account",
        "stale-demote",
        "store-mode",
        "unpin",
        "unplace",
        "vocabulary-update",
        "wrap-up",
    ];
    let mut have = names();
    have.sort_unstable();
    assert_eq!(have, expected);
}

/// F9: a carried typed predicate is a predicate `validate` runs. The bound
/// the port derives for this fixture's `RenderContract::generous` profile is
/// far below the contract's whole-request profile budget, so `admit-ingress`'s
/// `FITS` predicate re-holds at validate from the port-derived number - the
/// typed restatement, not a second gate and not a field only fixtures read.
#[test]
fn the_rows_typed_predicate_is_evaluated_at_validate() {
    use crate::context_kernel::legality::RenderContract;
    let mut ex = armed_executor(0);
    ex.propose("admit-ingress", 7).unwrap();
    ex.snapshot().unwrap();
    ex.generate().unwrap();
    let profile_budget = RenderContract::generous(1).profile_budget_units;
    // The region budget triple is carried at the profile's scale, so the
    // projected bound genuinely fits while still exercising the predicate.
    let budget = Budget {
        b: profile_budget,
        r: 8,
        h: 4,
    };
    let effect = profile_budget / 2; // far below any derived bound
    let bound = ex.port_bound(0, effect).expect("the port is bound");
    assert!(
        bound < profile_budget,
        "fixture must fit the profile budget"
    );
    let txn = ex.validate(bound, &budget, 80, 0, 0, effect).unwrap();
    assert_eq!(txn.op, "admit-ingress");
    assert_eq!(ex.state(), TxnState::Validated);
}

/// F9, failure half: the same carried predicate refuses an occupancy that
/// bursts the profile budget, through the registry row - not through a seam.
#[test]
fn a_row_predicate_failure_fails_validate_through_the_registry_row() {
    use crate::context_kernel::legality::RenderContract;
    let mut ex = armed_executor(0);
    ex.propose("admit-ingress", 7).unwrap();
    ex.snapshot().unwrap();
    ex.generate().unwrap();
    let profile_budget = RenderContract::generous(1).profile_budget_units;
    let effect = profile_budget * 2; // bursts the budget the row must fit
    let bound = ex.port_bound(0, effect).expect("the port is bound");
    let budget = Budget {
        b: profile_budget,
        r: 0,
        h: 0,
    };
    match ex.validate(bound, &budget, 4096, 0, 0, effect) {
        Err(ExecutorError::PreconditionFailed { which }) => {
            assert_eq!(which, "fit-bound");
        }
        other => panic!("the row's FITS predicate must refuse: {other:?}"),
    }
    assert_eq!(ex.state(), TxnState::Aborted);
}
