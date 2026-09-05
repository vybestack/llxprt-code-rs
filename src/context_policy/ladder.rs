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
    /// The index of this rung in the fixed escalation order.
    pub const fn index(self) -> usize {
        match self {
            Self::FoldAwayEphemeral => 0,
            Self::CollapsePlaceholders => 1,
            Self::DropWithHandle => 2,
            Self::Fold => 3,
            Self::Compact => 4,
            Self::Condense => 5,
        }
    }
}

/// How the managed region is degrading. Each class names its own designated
/// emergency rung: the escalation that answers THAT degradation first, before
/// the fixed order is resumed (issue 113).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum DegradationClass {
    /// Ephemera are drowning the region: fold away ephemeral records.
    EphemeralBuildup,
    /// Placeholder spans dominate: collapse placeholders into their digests.
    PlaceholderSaturation,
    /// Raw bytes the store could re-derive: drop them to durable handles.
    RederivableBulk,
    /// Redundant detail inside kept records: fold records together.
    RedundantDetail,
    /// Kept records carry stale or fragmented content: compact the region.
    FragmentedContent,
    /// Everything else: condense is the last general-purpose reclamation.
    GeneralDegradation,
}

impl DegradationClass {
    pub const fn all() -> [DegradationClass; 6] {
        [
            Self::EphemeralBuildup,
            Self::PlaceholderSaturation,
            Self::RederivableBulk,
            Self::RedundantDetail,
            Self::FragmentedContent,
            Self::GeneralDegradation,
        ]
    }
    /// The designated emergency rung for this degradation class: the first
    /// operation the ladder runs when the class escalates.
    pub const fn emergency_rung(self) -> Rung {
        match self {
            Self::EphemeralBuildup => Rung::FoldAwayEphemeral,
            Self::PlaceholderSaturation => Rung::CollapsePlaceholders,
            Self::RederivableBulk => Rung::DropWithHandle,
            Self::RedundantDetail => Rung::Fold,
            Self::FragmentedContent => Rung::Compact,
            Self::GeneralDegradation => Rung::Condense,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
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
        let emergency_choice = !scorer_available || low_confidence;
        // Issue 108-4 (F8): the row's emergency flag is consumed here - an
        // `Emergency` verdict may only be issued over a row the registry
        // flags emergency-capable. The scored path is untouched.
        if emergency_choice
            && !crate::context_txn::operation::find(rung.operation())
                .is_some_and(|row| row.emergency)
        {
            continue;
        }
        if emergency_choice {
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

/// Names the degradation the region is in, from the split a completed
/// transaction measured: bytes the record kept against bytes the store
/// reclaimed. A completion that reclaimed nothing while still admitting is
/// `RederivableBulk` (the kept bulk is the degradation); anything else keeps
/// `GeneralDegradation` unless a smaller measure names its own class.
pub fn degradation_class(kept: u64, reclaimed: u64) -> DegradationClass {
    if reclaimed == 0 {
        return DegradationClass::RederivableBulk;
    }
    let _ = kept;
    DegradationClass::GeneralDegradation
}

/// One step of the escalation ladder for a named degradation class.
///
/// Issue 113: `step` 0 is the class's own designated emergency rung (its
/// bytes answer THAT degradation first); after the emergency step the
/// selection continues from the NEXT rung in the fixed order — not a
/// restart — so repeated escalation walks the whole fixed order.
/// The terminal semantics are unchanged: `bound == 0` still refuses into
/// [`LadderChoice::Quiesce`], and `step >= bound` still hands over to
/// [`LadderChoice::WrapUp`].
pub fn escalate(step: usize, bound: usize, class: DegradationClass) -> LadderChoice {
    if bound == 0 {
        return LadderChoice::Quiesce;
    }
    if step >= bound {
        return LadderChoice::WrapUp;
    }
    if step == 0 {
        return LadderChoice::Emergency(class.emergency_rung());
    }
    let ladder = Rung::all();
    let emergency_index = class.emergency_rung().index();
    // After the emergency step, continue from the rung FOLLOWING the
    // emergency one in the fixed order, wrapping at the end of the ladder.
    let index = (emergency_index + step) % ladder.len();
    LadderChoice::Rung(ladder[index])
}
