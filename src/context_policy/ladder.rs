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
    if candidates.is_empty() {
        return LadderChoice::Quiesce;
    }
    for rung in Rung::all() {
        let capable = match rung {
            Rung::CollapsePlaceholders => caps.collapse_placeholders,
            Rung::DropWithHandle => caps.drop_with_handle,
            _ => true,
        };
        if !capable {
            continue;
        }
        let matching: Vec<&Candidate> = candidates.iter().filter(|c| c.rung == rung).collect();
        if matching.is_empty() {
            continue;
        }
        if armed {
            return LadderChoice::Emergency(rung);
        }
        let all_low_confidence = matching
            .iter()
            .all(|c| c.estimate.confidence < confidence_floor);
        if all_low_confidence {
            return LadderChoice::Rung(rung);
        }
        let _best = matching
            .iter()
            .max_by(|a, b| a.estimate.benefit.total_cmp(&b.estimate.benefit))
            .expect("matching rung is non-empty");
        return LadderChoice::Rung(rung);
    }
    let collapse_blocked = candidates
        .iter()
        .any(|c| c.rung == Rung::CollapsePlaceholders)
        && !caps.collapse_placeholders;
    if collapse_blocked && caps.drop_with_handle {
        return LadderChoice::Emergency(Rung::DropWithHandle);
    }
    LadderChoice::Quiesce
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
