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
