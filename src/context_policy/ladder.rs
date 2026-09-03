//! Fixed reclamation ladder with within-rung estimator ordering and emergency operations.

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
    pub const fn all() -> [Rung; 6] {
        [
            Self::FoldAwayEphemeral,
            Self::CollapsePlaceholders,
            Self::DropWithHandle,
            Self::Fold,
            Self::Compact,
            Self::Condense,
        ]
    }
    pub const fn operation(self) -> &'static str {
        match self {
            Self::FoldAwayEphemeral => "fold-away-ephemeral",
            Self::CollapsePlaceholders => "placeholder-collapse",
            Self::DropWithHandle => "drop-with-handle",
            Self::Fold => "fold",
            Self::Compact => "compact",
            Self::Condense => "condense",
        }
    }
}

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

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Estimate {
    pub benefit: f64,
    pub confidence: f64,
}
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LadderChoice {
    Rung(Rung),
    Emergency(Rung),
    WrapUp,
    Quiesce,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub choice: LadderChoice,
    pub candidate_index: Option<usize>,
}

pub fn select_candidate(
    candidates: &[Candidate],
    caps: &Capabilities,
    scorer_available: bool,
    confidence_floor: f64,
) -> Selection {
    for rung in Rung::all() {
        let capable = match rung {
            Rung::CollapsePlaceholders => caps.collapse_placeholders,
            Rung::DropWithHandle => caps.drop_with_handle,
            _ => true,
        };
        if !capable {
            continue;
        }
        let matching: Vec<(usize, &Candidate)> = candidates
            .iter()
            .enumerate()
            .filter(|(_, candidate)| candidate.rung == rung)
            .collect();
        if matching.is_empty() {
            continue;
        }
        let low_confidence = matching
            .iter()
            .all(|(_, candidate)| candidate.estimate.confidence < confidence_floor);
        if !scorer_available || low_confidence {
            return Selection {
                choice: LadderChoice::Emergency(rung),
                candidate_index: Some(matching[0].0),
            };
        }
        let best = matching
            .into_iter()
            .max_by(|(_, left), (_, right)| {
                left.estimate.benefit.total_cmp(&right.estimate.benefit)
            })
            .expect("matching rung is non-empty");
        return Selection {
            choice: LadderChoice::Rung(rung),
            candidate_index: Some(best.0),
        };
    }
    let collapse_blocked = candidates
        .iter()
        .any(|candidate| candidate.rung == Rung::CollapsePlaceholders)
        && !caps.collapse_placeholders;
    if collapse_blocked && caps.drop_with_handle {
        return Selection {
            choice: LadderChoice::Emergency(Rung::DropWithHandle),
            candidate_index: None,
        };
    }
    Selection {
        choice: LadderChoice::Quiesce,
        candidate_index: None,
    }
}

pub fn select(
    candidates: &[Candidate],
    caps: &Capabilities,
    scorer_available: bool,
    confidence_floor: f64,
) -> LadderChoice {
    select_candidate(candidates, caps, scorer_available, confidence_floor).choice
}

pub fn operation(choice: LadderChoice) -> Option<&'static str> {
    match choice {
        LadderChoice::Rung(rung) | LadderChoice::Emergency(rung) => Some(rung.operation()),
        LadderChoice::WrapUp => Some("wrap-up"),
        LadderChoice::Quiesce => Some("quiesce-unwritable"),
    }
}

pub fn escalate(step: usize, bound: usize) -> LadderChoice {
    if bound == 0 {
        return LadderChoice::Quiesce;
    }
    if step >= bound {
        return LadderChoice::WrapUp;
    }
    LadderChoice::Rung(Rung::all()[step % Rung::all().len()])
}
