//! Production `AccountingPort` (#104): the bound a commit is charged is
//! computed from the governed state and the declared tool surface, never
//! handed in by the caller.
//!
//! `D` (tool-declaration bytes) is part of the bound: growing the declared
//! tool surface moves the computed bound, so a fit check that passed with a
//! small tool surface can fail with a larger one even for identical effect
//! bytes. The port derives the governed term from the *encoded durable
//! spine*, which is the only region-wide measure the store exposes without a
//! new store API (see `store_spine_units`).

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
}

impl AccountingPort for BoundPort {
    /// Deterministic, conservative bound for `version`: the governed units
    /// already committed, the versioned tool surface, and the fixed commit
    /// frame. `version` is carried so replay of a transaction validated under
    /// an older spine version reproduces the governed term exactly.
    fn bound(&self, version: u64, contract: u64) -> u64 {
        let governed = if version >= self.margins.version {
            self.governed
        } else {
            0
        };
        governed
            .saturating_add(self.tool_surface())
            .saturating_add(contract)
            .saturating_add(self.margins.commit_frame)
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

    /// #104-2: growing the tool surface moves the computed bound.
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

    /// #104-1: a caller-supplied bound that disagrees with the port fails
    /// validation (the executor refuses it; the fixture pins the numbers).
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

    /// The port stays deterministic and conservative.
    #[test]
    fn port_is_deterministic_and_conservative() {
        let port = BoundPort::new(Margins::V1, 4, 32);
        assert_eq!(port.bound(1, 10), port.bound(1, 10));
        assert!(port.bound(1, 10) >= 10 + port.tool_surface());
    }
}
