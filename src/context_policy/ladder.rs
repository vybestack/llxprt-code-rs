//! Fixed reclamation ladder with capability adjustment, estimator reordering
//! restricted to a single rung, and bounded escalation to a terminal outcome.

/// Fixed ladder rungs in order (cheapest, most preferred first).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum Rung {
    FoldAwayEphemeral,
    CollapsePlaceholders,
    DropWithHandle,
    Fold,
    Compact,
    Condense,
}

impl Rung {
    pub fn all() -> [Rung; 6] {
        [
            Rung::FoldAwayEphemeral,
            Rung::CollapsePlaceholders,
            Rung::DropWithHandle,
            Rung::Fold,
            Rung::Compact,
            Rung::Condense,
        ]
    }
}

/// Executor capabilities that adjust rung selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub collapse_placeholders: bool,
    pub drop_with_handle: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            collapse_placeholders: true,
            drop_with_handle: true,
        }
    }
}

/// Scorer estimate for one candidate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Estimate {
    pub benefit: f64,
    pub confidence: f64,
}

/// One ladder candidate with its estimate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Candidate {
    pub rung: Rung,
    pub estimate: Estimate,
}

impl Candidate {
    pub const fn new(rung: Rung, benefit: f64, confidence: f64) -> Self {
        Self {
            rung,
            estimate: Estimate {
                benefit,
                confidence,
            },
        }
    }
}

/// Result of ladder selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LadderChoice {
    Rung(Rung),
    Emergency(Rung),
    WrapUp,
    Quiesce,
}

/// Select a rung. Estimator reordering may only move candidates within one
/// rung; low confidence or scorer outage falls back to the fixed order.
pub fn select(
    candidates: &[Candidate],
    caps: &Capabilities,
    armed: bool,
    confidence_floor: f64,
) -> LadderChoice {
    // RED: picks by highest estimated benefit, ignoring capability adjustment,
    // the confidence floor (scorer outage), and the armed economics.
    let _ = (caps, armed, confidence_floor);
    match candidates
        .iter()
        .max_by(|a, b| a.estimate.benefit.total_cmp(&b.estimate.benefit))
    {
        Some(best) => LadderChoice::Rung(best.rung),
        None => LadderChoice::Quiesce,
    }
}

/// Bounded escalation: always terminates in wrap-up or quiesce, never an armed
/// unquiesced no-op.
pub fn escalate(step: usize, bound: usize) -> LadderChoice {
    if bound == 0 {
        return LadderChoice::Quiesce;
    }
    if step >= bound {
        return LadderChoice::WrapUp;
    }
    let ladder = Rung::all();
    LadderChoice::Rung(ladder[step % ladder.len()])
}
