//! Versioned margin table (#104).
//!
//! A margin is the byte cost the executor charges to a commit beyond the
//! effect itself: the fixed commit frame and, since #104, the
//! tool-declaration surface `D`. Margins are *versioned* so replay of an old
//! transaction reproduces the bound it was validated under; a version bump is
//! the mechanism a recalibration uses to record drift.
//!
//! Usage reconciliation itself is blocked by #80 (no measured per-request
//! accounting reaches the executor today); what this module lands is the
//! versioning plus the drift fixture so the moment #80 unblocks, the measured
//! number has a version to be compared against.

/// One version of the margin table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Margins {
    /// Monotonic table version; a drift fixture bumps it via [`Self::recalibrate`].
    pub version: u64,
    /// Per-tool-declaration charge (the `D` term of the bound).
    pub per_tool_declaration: u64,
    /// Fixed frame overhead charged to every commit (record header + framing).
    pub commit_frame: u64,
}

impl Margins {
    pub const V1: Margins = Margins {
        version: 1,
        per_tool_declaration: 32,
        commit_frame: 64,
    };

    /// Recalibrates on detected drift: a strictly newer version with the
    /// measured per-declaration cost. An older or equal version is rejected
    /// because replay must never silently adopt a newer margin table than the
    /// one it was validated under.
    pub fn recalibrate(
        self,
        version: u64,
        per_tool_declaration: u64,
    ) -> Result<Self, &'static str> {
        if version <= self.version {
            return Err("margin version must be strictly newer");
        }
        Ok(Margins {
            version,
            per_tool_declaration,
            commit_frame: self.commit_frame,
        })
    }

    /// Byte cost charged to a commit for the declared tool surface.
    pub fn tool_surface(&self, declarations: usize) -> u64 {
        self.per_tool_declaration
            .saturating_mul(declarations.min(u32::MAX as usize) as u64)
    }
}

// Budget accounting port and the universal-commit predicates (design §8.3).
//
// `B` is the region budget, `R` the reclamation reserve and `H` the headroom
// the executor must leave free so reclamation can always run.

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
#[derive(Clone, Copy, PartialEq, Eq)]
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

#[cfg(test)]
mod tests {
    use super::*;

    /// #104-3: a margin-drift fixture raises the per-declaration charge and
    /// bumps the version; a stale version never wins.
    #[test]
    fn margin_drift_fixture_recalibrates_under_a_new_version() {
        let v1 = Margins::V1;
        let drifted = v1.recalibrate(2, 128).expect("newer version recalibrates");
        assert_eq!(drifted.version, 2);
        assert_eq!(drifted.per_tool_declaration, 128);
        assert!(drifted.tool_surface(4) > v1.tool_surface(4));
        assert!(v1.recalibrate(1, 999).is_err());
        assert!(v1.recalibrate(0, 999).is_err());
    }
}
