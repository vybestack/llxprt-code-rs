//! Budget accounting port and the universal-commit predicates (design §8.3).
//!
//! `B` is the region budget, `R` the reclamation reserve and `H` the headroom
//! the executor must leave free so reclamation can always run.

/// Read-only port the executor uses to ask for a region bound.
///
/// Contract of any implementation:
/// * **deterministic** - the bound for `(version, contract)` is a pure
///   function of those two inputs, so replay reproduces it exactly;
/// * **additive** - bounds compose across regions: the bound of a disjoint
///   union is the sum of the member bounds (no double counting);
/// * **conservative** - the port never under-reports a bound, so a passing
///   fit check implies the real consumption also fits.
pub trait AccountingPort {
    /// Conservative bound for `version` under `contract`, in budget units.
    fn bound(&self, version: u64, contract: u64) -> u64;
}

/// Region budget triple.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Region budget.
    pub b: u64,
    /// Reclamation reserve.
    pub r: u64,
    /// Headroom that must stay free.
    pub h: u64,
}

impl Budget {
    /// The largest bound a commit may reach: `B - R - H`.
    pub fn commit_ceiling(&self) -> u64 {
        self.b.saturating_sub(self.r).saturating_sub(self.h)
    }
}

/// Universal precondition: projected fit must satisfy `bound(v', c) <= B-R-H`.
pub fn fits(bound: u64, budget: &Budget) -> bool {
    bound <= budget.commit_ceiling()
}

/// Universal precondition: protection floor `M + R + H <= B` (terminal reserve
/// lives inside `M`).
pub fn feasible(m: u64, budget: &Budget) -> bool {
    m.saturating_add(budget.r).saturating_add(budget.h) <= budget.b
}

/// Universal precondition for reclamation-class rows: the potential must not
/// increase, and must drop by at least `bar` at the validated state.
pub fn net_reclaim_ok(phi_pre: u64, phi_post: u64, bar: u64) -> bool {
    phi_post <= phi_pre && phi_pre - phi_post >= bar
}
