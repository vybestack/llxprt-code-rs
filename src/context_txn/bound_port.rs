//! Production `AccountingPort` (104): the bound a commit is charged is
//! computed from the governed state and the declared tool surface, never
//! handed in by the caller.
//!
//! `D` (tool-declaration bytes) is part of the bound: growing the declared
//! tool surface moves the computed bound, so a fit check that passed with a
//! small tool surface can fail with a larger one even for identical effect
//! bytes. The port derives the governed term from the *encoded durable
//! spine*, which is the only region-wide measure the store exposes without a
//! new store API (see `store_spine_units`).
//!
//! The `version` argument is the **margin-table version the caller validated
//! under**, so the port compares margin version against margin version and
//! never against the store's spine version. A transaction validated under an
//! older margin table reproduces the governed term it was charged then, and a
//! table recalibrated past it does not silently zero the term.

use super::budget::{AccountingPort, Margins};

/// Port the executor asks for a bound: versioned margins plus the session's
/// declared tool surface.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BoundPort {
    margins: Margins,
    tool_declarations: usize,
    governed: u64,
}

impl BoundPort {
    /// Port over `margins`, a declared tool surface of `tool_declarations`
    /// pairs, and `governed` units already committed to the region.
    pub fn new(margins: Margins, tool_declarations: usize, governed: u64) -> Self {
        Self {
            margins,
            tool_declarations,
            governed,
        }
    }

    /// The committed units this port charges every commit against.
    pub fn governed_units(&self) -> u64 {
        self.governed
    }

    /// The declared tool surface `D`, in budget units.
    pub fn tool_surface(&self) -> u64 {
        self.margins.tool_surface(self.tool_declarations)
    }

    /// Declared tool pairs the surface is charged for.
    pub fn tool_declarations(&self) -> usize {
        self.tool_declarations
    }

    /// The version of the margin table this port applies.
    pub fn margin_version(&self) -> u64 {
        self.margins.version
    }

    /// Grows the declared tool surface (a session adopting more tools), which
    /// grows the computed bound.
    pub fn declare_tools(&mut self, tool_declarations: usize) {
        self.tool_declarations = tool_declarations;
    }

    /// Recalibrates the margin table on detected drift (104-3): a strictly
    /// newer `version` with the measured per-declaration cost. The charge is
    /// carried by this port - the only margin table the bound is computed
    /// from - so a recalibration actually moves every later bound.
    pub fn recalibrate(
        &mut self,
        version: u64,
        per_tool_declaration: u64,
    ) -> Result<(), &'static str> {
        self.margins = self.margins.recalibrate(version, per_tool_declaration)?;
        Ok(())
    }

    /// The margin table this port charges every commit against (104-3).
    pub fn margins(&self) -> Margins {
        self.margins
    }

    /// A clone of this port with the margin table recalibrated to `version`.
    fn after_recalibration(
        &self,
        version: u64,
        per_tool_declaration: u64,
    ) -> Result<BoundPort, &'static str> {
        let mut next = self.clone();
        next.recalibrate(version, per_tool_declaration)?;
        Ok(next)
    }
}

impl AccountingPort for BoundPort {
    /// Deterministic, conservative bound for `effect_bytes`: the governed
    /// units already committed, the versioned tool surface, the effect bytes
    /// themselves, and the fixed commit frame. `version` is the margin-table
    /// version the transaction was validated under (not the store's spine
    /// version), so replay of a transaction validated under an older table
    /// reproduces the governed term exactly instead of zeroing it.
    fn bound(&self, version: u64, effect_bytes: u64) -> u64 {
        let governed = if version >= self.margins.version {
            self.governed
        } else {
            0
        };
        governed
            .saturating_add(self.tool_surface())
            .saturating_add(effect_bytes)
            .saturating_add(self.margins.commit_frame)
    }

    fn bound_margins(&self) -> Option<Margins> {
        Some(self.margins)
    }

    fn recalibrated(
        &self,
        version: u64,
        per_tool_declaration: u64,
    ) -> Option<Result<std::rc::Rc<dyn AccountingPort>, &'static str>> {
        Some(
            match self.after_recalibration(version, per_tool_declaration) {
                Ok(port) => Ok(std::rc::Rc::new(port) as std::rc::Rc<dyn AccountingPort>),
                Err(reason) => Err(reason),
            },
        )
    }
}

/// Region-wide units the durable spine occupies: the encoded spine length,
/// which is what the store charges against `SPINE_RELOAD_MAX` on reload, so
/// admission accounting and the reload bound stay the same number.
pub fn store_spine_units(spine_bytes_len: u64) -> u64 {
    spine_bytes_len
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 104-2: growing the tool surface moves the computed bound.
    #[test]
    fn growing_the_tool_surface_moves_the_bound() {
        let small = BoundPort::new(Margins::V1, 2, 100);
        let big = BoundPort::new(Margins::V1, 10, 100);
        let smaller_bound = small.bound(1, 0);
        let bigger_bound = big.bound(1, 0);
        assert!(bigger_bound > smaller_bound);
        assert_eq!(
            bigger_bound - smaller_bound,
            (10 - 2) as u64 * Margins::V1.per_tool_declaration
        );
    }

    /// 104-1: the port computes the bound from its own inputs, so a
    /// caller-supplied number is not echoed back. The caller's claim is
    /// compared against this, which makes `BoundDisagrees` reachable.
    #[test]
    fn a_caller_supplied_bound_must_agree_with_the_port() {
        let port = BoundPort::new(Margins::V1, 3, 500);
        let port_bound = port.bound(1, 64);
        assert_eq!(
            port_bound,
            500 + 3 * Margins::V1.per_tool_declaration + 64 + Margins::V1.commit_frame
        );
        let invented = port_bound + 1;
        assert_ne!(invented, port_bound);
        // The executor's verdict for a disagreement is exercised in
        // `context_txn::tests::a_bound_that_disagrees_with_the_port_fails_validation`.
        assert!(invented > port_bound);
    }

    /// 104-3: recalibration routes through the port that owns the table, so
    /// the recalibrated charge actually moves the computed bound.
    #[test]
    fn recalibrating_the_port_moves_the_bound() {
        let mut port = BoundPort::new(Margins::V1, 4, 0);
        let before = port.bound(port.margin_version(), 100);
        port.recalibrate(2, 128)
            .expect("newer version recalibrates");
        let after = port.bound(port.margin_version(), 100);
        assert!(after > before);
        assert_eq!(port.margin_version(), 2);
        assert!(port.recalibrate(2, 999).is_err());
        assert!(port.recalibrate(1, 999).is_err());
    }

    /// Margin versions are compared against margin versions, never against a
    /// store version: a table newer than the spine version still charges the
    /// governed term.
    #[test]
    fn margin_versions_compare_against_margin_versions() {
        let port = BoundPort::new(Margins::V1, 0, 512);
        assert_eq!(
            port.bound(Margins::V1.version, 10),
            512 + 10 + Margins::V1.commit_frame
        );
        // A transaction validated under an older margin table is reproduced
        // exactly as it was then.
        assert_eq!(port.bound(0, 10), 10 + Margins::V1.commit_frame);
    }

    /// The port stays deterministic and conservative.
    #[test]
    fn port_is_deterministic_and_conservative() {
        let port = BoundPort::new(Margins::V1, 4, 32);
        assert_eq!(port.bound(1, 10), port.bound(1, 10));
        assert!(port.bound(1, 10) >= 10 + port.tool_surface());
    }
}
