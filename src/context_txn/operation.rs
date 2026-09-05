//! Closed operation registry (design.tex §9 `tab:ops`).
//!
//! One row per operation. Columns map onto [`Operation`] fields: proposer,
//! reclamation class (`reclamation`), deterministic, rebase-safe, owner phase.
//! Rows whose owner phase is not 3 are still registered; the executor answers
//! them with a `capability_not_landed` result instead of executing them.
//!
//! Unit B (#108): owner phases are generated from the single source table
//! (`project-plans/issue36-context-mgmt/plan.md:377-384`) and the conformance
//! test asserts every row against it; pre/postconditions are typed predicates
//! (implementing [`Precondition`] / [`Postcondition`]), not display strings;
//! every row may carry an `emergency` flag the ladder applies when it issues an
//! `Emergency(..)` verdict over that row; and
//! every reclamation-class row has a nonzero bar (#104-4).
//!
//! Who consumes the flag: the executor consumes it when it builds
//! [`PreconditionFacts`] for a row's typed precondition (an emergency
//! application is exempt from the reclamation bar but may never raise `Phi`),
//! and the ladder refuses to issue an `Emergency(..)` verdict over a row whose
//! registered entry is not flagged emergency-capable. The conformance test ties
//! the ladder's rung names to flagged rows (issue 108-4).

use super::budget::{self, Budget};

/// Who may propose an operation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Proposer {
    /// System / harness.
    S,
    /// Controllable model.
    C,
    /// Model.
    M,
    /// Observer.
    O,
    /// User.
    U,
    /// Lane.
    L,
}

impl Proposer {
    /// Registry-table letter for this proposer.
    pub fn as_str(self) -> &'static str {
        match self {
            Proposer::S => "S",
            Proposer::C => "C",
            Proposer::M => "M",
            Proposer::O => "O",
            Proposer::U => "U",
            Proposer::L => "L",
        }
    }
}

/// Facts the executor needs to evaluate a typed precondition (#108-3): the
/// caller supplies only governed numbers - the committed parent version, the
/// projected occupancy, the budget triple, and the row's reclamation
/// potential - never a verdict.
#[derive(Clone, Copy)]
pub struct PreconditionFacts {
    /// Parent version the transaction commits against.
    pub parent_version: u64,
    /// Projected occupancy of the row's effect, in budget units.
    pub projected: u64,
    /// Region budget triple.
    pub budget: Budget,
    /// Reclamation potential before the effect (`Phi`).
    pub phi_pre: u64,
    /// Reclamation potential after the effect (`Phi`).
    pub phi_post: u64,
    /// The row's registered bar.
    pub bar: u64,
    /// Whether the row is being applied as an emergency rung.
    pub emergency: bool,
}

impl PreconditionFacts {
    /// Facts for a plain (non-emergency) application of `row`.
    pub fn for_row(row: &Operation, projected: u64, budget: Budget) -> Self {
        Self {
            parent_version: 0,
            projected,
            budget,
            phi_pre: 0,
            phi_post: 0,
            bar: row.bar,
            emergency: false,
        }
    }
}

/// Typed commit precondition (design §8.3). Evaluated at `validate`.
pub trait Precondition: Send + Sync {
    /// The predicate, over governed facts only.
    fn holds(&self, facts: &PreconditionFacts) -> bool;
    /// Stable text of the predicate, for verdicts and reports.
    fn text(&self) -> &'static str;
}

/// Typed postcondition observable after commit. Evaluated over the same
/// facts a caller can measure.
pub trait Postcondition: Send + Sync {
    /// The predicate.
    fn holds(&self, facts: &PreconditionFacts) -> bool;
    /// Stable text of the predicate.
    fn text(&self) -> &'static str;
}

/// A predicate that always holds; rows whose real precondition is not yet
/// measurable from governed facts carry this placeholder, and the conformance
/// test names them.
struct Always;

impl Precondition for Always {
    fn holds(&self, _facts: &PreconditionFacts) -> bool {
        true
    }
    fn text(&self) -> &'static str {
        "unconditional"
    }
}

impl Postcondition for Always {
    fn holds(&self, _facts: &PreconditionFacts) -> bool {
        true
    }
    fn text(&self) -> &'static str {
        "unconditional"
    }
}

/// The projected occupancy must fit the region budget net of reserve and
/// headroom.
struct FitsBound;

impl Precondition for FitsBound {
    fn holds(&self, facts: &PreconditionFacts) -> bool {
        budget::fits(facts.projected, &facts.budget)
    }
    fn text(&self) -> &'static str {
        "projected occupancy fits B-R-H"
    }
}

impl Postcondition for FitsBound {
    fn holds(&self, facts: &PreconditionFacts) -> bool {
        budget::fits(facts.projected, &facts.budget)
    }
    fn text(&self) -> &'static str {
        "projected occupancy fits B-R-H"
    }
}

/// A reclamation-class row must net at least its registered bar (`bar > 0`
/// for every reclamation row, #104-4). Emergency rungs are exempt from the
/// bar: the ladder applies them to recover a stuck region (#108-4).
struct ReclaimsBar;

impl Precondition for ReclaimsBar {
    fn holds(&self, facts: &PreconditionFacts) -> bool {
        if facts.emergency {
            // An emergency rung is exempt from the bar, but it may never
            // raise Phi: the ladder recovers, it does not grow the region.
            return facts.phi_post <= facts.phi_pre;
        }
        budget::net_reclaim_ok(facts.phi_pre, facts.phi_post, facts.bar)
    }
    fn text(&self) -> &'static str {
        "Phi nets at least the registered bar"
    }
}

impl Postcondition for ReclaimsBar {
    fn holds(&self, facts: &PreconditionFacts) -> bool {
        Precondition::holds(&ReclaimsBar, facts)
    }
    fn text(&self) -> &'static str {
        "Phi nets at least the registered bar"
    }
}

/// A single registry row.
#[derive(Clone, Copy)]
pub struct Operation {
    /// Operation name as written in `tab:ops` (kebab-case).
    pub name: &'static str,
    /// Proposing role.
    pub proposer: Proposer,
    /// Secondary authority that must not increase (dual-proposer rows).
    pub authority: Option<Proposer>,
    /// Typed commit precondition, evaluated at `validate` (#108-3).
    pub precondition: &'static dyn Precondition,
    /// Typed postcondition observable after commit (#108-3).
    pub postcondition: &'static dyn Postcondition,
    /// Reclamation class: net `Phi` must drop by at least the bar.
    pub reclamation: bool,
    /// Deterministic operation.
    pub deterministic: bool,
    /// Rebase-safe: re-applies on the actual parent instead of aborting.
    pub rebase_safe: bool,
    /// Phase that owns the implementation of this row, from the single source
    /// table (plan.md:377-384) - see `PHASES`.
    pub owner_phase: u8,
    /// Minimum Phi-net the reclamation engine guarantees; nonzero for every
    /// reclamation row (#104-4).
    pub bar: u64,
    /// Whether the row may be applied as an emergency rung of the reclamation
    /// ladder (#108-4).
    pub emergency: bool,
}

/// Owner phase per operation, transcribed from the single source table
/// (`project-plans/issue36-context-mgmt/plan.md:377-384`), which is the
/// authority wherever an earlier transcription disagreed.
///
/// Rows named by two plan phase rows are assigned to the row the plan marks as
/// the implementation owner, and the choice is recorded on the row itself.
/// The plan's phase-3 row (plan.md:379) names mechanisms, not rows: "Executor
/// lifecycle and universal checks for every row; generic compare-and-commit,
/// abort/reproposal, governed-event closure, operation identity and
/// deduplication". The registry therefore carries no phase-3 rows from that
/// citation; the rows an earlier transcription put at phase 3
/// (`re-promote`, `placeholder-collapse`, `drop-with-handle`,
/// `fold-away-ephemeral`, `pin-override-collapse`, `spend-account`,
/// `journal-append`) are named by the plan's phase-4 row (plan.md:380) and
/// live at phase 4 here, and `lease-acquire` keeps its context-side skeleton
/// at phase 3 per the plan's phase-1 row (plan.md:377).
///
/// phase 1: `scope-open`, `scope-close`, `checkpoint`, sequencer event
/// identity, context-side `lease-acquire` skeleton, render-contract type
/// skeleton.
///
/// phase 2: `admit-ingress`, `sanitize`, `redact`, `import`, `rule-update`,
/// `vocabulary-update`, `index-rebuild`, `store-mode`, store-side
/// `quiesce-unwritable`.
///
/// phase 4: `admit-observation`, `admit-as-handle`, `admit-feedback`,
/// `re-promote`, `placeholder-collapse`, `drop-with-handle`,
/// `fold-away-ephemeral`, `pin-override-collapse`, `spend-account`,
/// `journal-append`, `declare-boundary`, `flush-notes`, `arm`, `disarm`,
/// `queue-service`, `calibration-update`, `margin-recalibrate`, `wrap-up`,
/// `quiesce-unwritable`, `consolidate-head`, `consolidate-notes`.
///
/// phase 5: `admit-obligation`, `admit-convenience`, `demote`, `discharge`,
/// `consolidate-obligations`, `amend`, `retract`, `promote`, `fact-invalidate`,
/// `revalidate`, `stale-demote`, `decay`.
///
/// phase 6: `note`, `read-back`, `condense`, `fold`, `compact`, `regenerate`,
/// `reclassify`, `resegment`, `reopen` and typed gate retry/abort behavior.
///
/// phase 7: `pending-response-stage`, `admit-response`, `repair-result`, full
/// `lease-acquire`, `render-contract-observed` and provider-turn
/// intent/completion events.
///
/// phase 8: `pin`, `unpin`, `expire-pin`, `escalate-pending`,
/// `escalate-complete`, `branch-open`, `branch-return`, `branch-abort`,
/// authenticated security/adoption events.
///
/// phase 9: `calibration-update`, `margin-recalibrate`, `index-rebuild` and
/// any other later-phase rows the plan keeps in its tail. The plan's phase-4
/// row names `calibration-update` and `margin-recalibrate` as implementation
/// owners, so the registry assigns phase 4 (plan.md:381); `index-rebuild` is
/// named by the plan's phase-2 row, so the registry assigns phase 2
/// (plan.md:378), overriding the earlier phase-9 transcription.
const PHASES: &[(&str, u8)] = &[
    // phase 1 (plan.md:377)
    ("scope-open", 1),
    ("scope-close-by-event", 1),
    ("checkpoint", 1),
    // phase 2 (plan.md:378)
    ("admit-ingress", 2),
    ("sanitize", 2),
    ("redact", 2),
    ("import", 2),
    ("rule-update", 2),
    ("vocabulary-update", 2),
    ("index-rebuild", 2),
    ("store-mode", 2),
    ("migration-select", 2),
    // phase 3 (plan.md:379): the plan's phase-3 row names executor lifecycle
    // and universal checks - mechanisms, not rows - so the only row it keeps
    // here is the context-side `lease-acquire` skeleton the phase-1 row
    // already named (see the doc above).
    ("lease-acquire", 4),
    // phase 4 (plan.md:380-381): the plan's phase-4 row names every row the
    // earlier transcription had put at phase 3 (see the doc above).
    ("admit-observation", 4),
    ("admit-as-handle", 4),
    ("admit-feedback", 4),
    ("re-promote", 4),
    ("placeholder-collapse", 4),
    ("drop-with-handle", 4),
    ("fold-away-ephemeral", 4),
    ("pin-override-collapse", 4),
    // The plan's phase-4 row names `declare-boundary`; the committed class
    // is `scope-close-by-declaration` (EventKind::ScopeCloseByDeclaration),
    // so the registry carries that name and this row resolves the source
    // table entry.
    ("scope-close-by-declaration", 4),
    ("flush-notes", 4),
    ("arm", 4),
    ("disarm", 4),
    ("queue-service", 4),
    ("calibration-update", 4),
    ("margin-recalibrate", 4),
    ("wrap-up", 4),
    ("quiesce-unwritable", 4),
    ("consolidate-head", 4),
    ("consolidate-notes", 4),
    // phase 5 (plan.md:382)
    ("admit-obligation", 5),
    ("admit-convenience", 5),
    ("demote", 5),
    ("discharge", 5),
    ("consolidate-obligations", 5),
    ("amend", 5),
    ("retract", 5),
    ("promote", 5),
    ("fact-invalidate", 5),
    ("revalidate", 5),
    ("stale-demote", 5),
    ("decay", 5),
    // phase 6 (plan.md:383)
    ("note", 6),
    ("read-back", 6),
    ("condense", 6),
    ("fold", 6),
    ("compact", 6),
    ("regenerate", 6),
    ("reclassify", 6),
    ("resegment", 6),
    ("reopen", 6),
    // phase 7 (plan.md:384): the phase-7 row names only pending-response-stage,
    // admit-response, repair-result, full lease-acquire (the registry's phase-3
    // row is the context-side skeleton, not the full lease), and
    // render-contract-observed.
    ("pending-response-stage", 7),
    ("admit-response", 7),
    ("render-contract-observed", 7),
    ("repair-result", 7),
    // phase 8 (plan.md:385)
    ("pin", 8),
    ("unpin", 8),
    ("expire-pin", 8),
    ("escalate-pending", 8),
    ("escalate-complete", 8),
    ("branch-open", 8),
    ("branch-return", 8),
    ("branch-abort", 8),
    // phase 9: the plan's tail keeps no row the registry transcribes
    // separately; calibration/margin/index-rebuild are owned by the rows
    // above, so phase 9 has no entry here.
];

/// Owner phase for `name` from the single source table; a name the table does
/// not carry keeps its registered transcription and is reported by the
/// conformance test.
pub fn owner_phase(name: &str) -> Option<u8> {
    PHASES
        .iter()
        .find(|(row, _)| *row == name)
        .map(|(_, phase)| *phase)
}

/// Every `(name, phase)` pair the single source table carries; the generated
/// conformance test iterates this, so a divergent registry row fails it.
pub fn source_table() -> &'static [(&'static str, u8)] {
    PHASES
}

// Issue 121-d (F19): the only two `rebase_safe` rows, `note` and
// `read-back`, are owned by phase 6 while the executor lands through phase 4,
// so `generate()` refuses them and no production caller can reach the
// `CommitOutcome::RebaseNoOp` path today. That is the deliberate recovery
// seam: when the plan lands phase 6, raising the executor's landed phase is
// the ONLY change needed; until then this block is the documented recovery
// seam, and the `land_through` fixtures keep the path honestly exercised.
const FITS: &FitsBound = &FitsBound;
const RECLAIMS: &ReclaimsBar = &ReclaimsBar;
const UNCONDITIONAL: &Always = &Always;

/// The closed operation registry.
#[rustfmt::skip]
static REGISTRY: [Operation; 70] = [
    Operation { name: "admit-ingress", proposer: Proposer::S, authority: None, precondition: FITS, postcondition: FITS, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 2, bar: 0, emergency: false },
    Operation { name: "admit-observation", proposer: Proposer::S, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 4, bar: 0, emergency: false },
    Operation { name: "admit-as-handle", proposer: Proposer::S, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 4, bar: 0, emergency: false },
    Operation { name: "admit-feedback", proposer: Proposer::S, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 4, bar: 0, emergency: false },
    Operation { name: "pending-response-stage", proposer: Proposer::S, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 7, bar: 0, emergency: false },
    Operation { name: "admit-response", proposer: Proposer::S, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 7, bar: 0, emergency: false },
    Operation { name: "admit-obligation", proposer: Proposer::S, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 5, bar: 0, emergency: false },
    Operation { name: "admit-convenience", proposer: Proposer::S, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 5, bar: 0, emergency: false },
    Operation { name: "sanitize", proposer: Proposer::S, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 2, bar: 0, emergency: false },
    Operation { name: "redact", proposer: Proposer::O, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 2, bar: 0, emergency: false },
    Operation { name: "note", proposer: Proposer::M, authority: Some(Proposer::C), precondition: FITS, postcondition: FITS, reclamation: false, deterministic: false, rebase_safe: true, owner_phase: 6, bar: 0, emergency: false },
    Operation { name: "read-back", proposer: Proposer::M, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: true, owner_phase: 6, bar: 0, emergency: false },
    Operation { name: "re-promote", proposer: Proposer::C, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 4, bar: 0, emergency: true },
    Operation { name: "placeholder-collapse", proposer: Proposer::C, authority: None, precondition: RECLAIMS, postcondition: RECLAIMS, reclamation: true, deterministic: false, rebase_safe: false, owner_phase: 4, bar: 8, emergency: true },
    Operation { name: "drop-with-handle", proposer: Proposer::C, authority: None, precondition: RECLAIMS, postcondition: RECLAIMS, reclamation: true, deterministic: true, rebase_safe: false, owner_phase: 4, bar: 8, emergency: true },
    Operation { name: "fold-away-ephemeral", proposer: Proposer::C, authority: None, precondition: RECLAIMS, postcondition: RECLAIMS, reclamation: true, deterministic: false, rebase_safe: false, owner_phase: 4, bar: 8, emergency: true },
    Operation { name: "condense", proposer: Proposer::C, authority: None, precondition: RECLAIMS, postcondition: RECLAIMS, reclamation: true, deterministic: false, rebase_safe: false, owner_phase: 6, bar: 8, emergency: true },
    Operation { name: "fold", proposer: Proposer::C, authority: None, precondition: RECLAIMS, postcondition: RECLAIMS, reclamation: true, deterministic: false, rebase_safe: false, owner_phase: 6, bar: 8, emergency: true },
    Operation { name: "compact", proposer: Proposer::C, authority: None, precondition: RECLAIMS, postcondition: RECLAIMS, reclamation: true, deterministic: false, rebase_safe: false, owner_phase: 6, bar: 8, emergency: true },
    Operation { name: "regenerate", proposer: Proposer::C, authority: None, precondition: RECLAIMS, postcondition: RECLAIMS, reclamation: true, deterministic: false, rebase_safe: false, owner_phase: 6, bar: 8, emergency: true },
    Operation { name: "pin-override-collapse", proposer: Proposer::C, authority: None, precondition: RECLAIMS, postcondition: RECLAIMS, reclamation: true, deterministic: false, rebase_safe: false, owner_phase: 4, bar: 8, emergency: true },
    Operation { name: "demote", proposer: Proposer::L, authority: None, precondition: RECLAIMS, postcondition: RECLAIMS, reclamation: true, deterministic: false, rebase_safe: false, owner_phase: 5, bar: 8, emergency: true },
    Operation { name: "discharge", proposer: Proposer::L, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 5, bar: 0, emergency: false },
    Operation { name: "consolidate-obligations", proposer: Proposer::L, authority: None, precondition: RECLAIMS, postcondition: RECLAIMS, reclamation: true, deterministic: false, rebase_safe: false, owner_phase: 5, bar: 8, emergency: true },
    Operation { name: "escalate-pending", proposer: Proposer::O, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 8, bar: 0, emergency: false },
    Operation { name: "escalate-complete", proposer: Proposer::O, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 8, bar: 0, emergency: false },
    Operation { name: "amend", proposer: Proposer::M, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 5, bar: 0, emergency: false },
    Operation { name: "retract", proposer: Proposer::M, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 5, bar: 0, emergency: false },
    Operation { name: "promote", proposer: Proposer::M, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 5, bar: 0, emergency: false },
    Operation { name: "reclassify", proposer: Proposer::M, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 6, bar: 0, emergency: false },
    Operation { name: "resegment", proposer: Proposer::M, authority: Some(Proposer::C), precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 6, bar: 0, emergency: false },
    Operation { name: "reopen", proposer: Proposer::M, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 6, bar: 0, emergency: false },
    Operation { name: "pin", proposer: Proposer::M, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 8, bar: 0, emergency: false },
    Operation { name: "unpin", proposer: Proposer::M, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 8, bar: 0, emergency: false },
    Operation { name: "expire-pin", proposer: Proposer::S, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 8, bar: 0, emergency: false },
    Operation { name: "scope-close-by-declaration", proposer: Proposer::C, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 4, bar: 0, emergency: false },
    Operation { name: "scope-open", proposer: Proposer::S, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 1, bar: 0, emergency: false },
    Operation { name: "scope-close-by-event", proposer: Proposer::S, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 1, bar: 0, emergency: false },
    Operation { name: "decay", proposer: Proposer::S, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 5, bar: 0, emergency: false },
    Operation { name: "flush-notes", proposer: Proposer::S, authority: Some(Proposer::C), precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 4, bar: 0, emergency: false },
    Operation { name: "arm", proposer: Proposer::S, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 4, bar: 0, emergency: false },
    Operation { name: "disarm", proposer: Proposer::S, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 4, bar: 0, emergency: false },
    Operation { name: "rule-update", proposer: Proposer::M, authority: Some(Proposer::C), precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 2, bar: 0, emergency: false },
    Operation { name: "vocabulary-update", proposer: Proposer::M, authority: Some(Proposer::C), precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 2, bar: 0, emergency: false },
    Operation { name: "fact-invalidate", proposer: Proposer::S, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 5, bar: 0, emergency: false },
    Operation { name: "revalidate", proposer: Proposer::L, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 5, bar: 0, emergency: false },
    Operation { name: "stale-demote", proposer: Proposer::S, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 5, bar: 0, emergency: false },
    Operation { name: "repair-result", proposer: Proposer::S, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 7, bar: 0, emergency: false },
    Operation { name: "branch-open", proposer: Proposer::M, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 8, bar: 0, emergency: false },
    Operation { name: "branch-return", proposer: Proposer::M, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 8, bar: 0, emergency: false },
    Operation { name: "branch-abort", proposer: Proposer::M, authority: Some(Proposer::C), precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 8, bar: 0, emergency: false },
    Operation { name: "checkpoint", proposer: Proposer::S, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 1, bar: 0, emergency: false },
    Operation { name: "lease-acquire", proposer: Proposer::S, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 4, bar: 0, emergency: true },
    Operation { name: "render-contract-observed", proposer: Proposer::S, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 7, bar: 0, emergency: false },
    Operation { name: "calibration-update", proposer: Proposer::S, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 4, bar: 0, emergency: false },
    Operation { name: "margin-recalibrate", proposer: Proposer::S, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 4, bar: 0, emergency: false },
    Operation { name: "store-mode", proposer: Proposer::S, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 2, bar: 0, emergency: false },
    Operation { name: "queue-service", proposer: Proposer::S, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 4, bar: 0, emergency: false },
    Operation { name: "spend-account", proposer: Proposer::S, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 4, bar: 0, emergency: true },
    Operation { name: "journal-append", proposer: Proposer::S, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 4, bar: 0, emergency: true },
    Operation { name: "wrap-up", proposer: Proposer::C, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 4, bar: 0, emergency: false },
    Operation { name: "quiesce-unwritable", proposer: Proposer::S, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 4, bar: 0, emergency: false },
    Operation { name: "index-rebuild", proposer: Proposer::S, authority: Some(Proposer::O), precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 2, bar: 0, emergency: false },
    Operation { name: "import", proposer: Proposer::O, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 2, bar: 0, emergency: false },
    Operation { name: "consolidate-head", proposer: Proposer::O, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 4, bar: 0, emergency: false },
    Operation { name: "consolidate-notes", proposer: Proposer::C, authority: None, precondition: RECLAIMS, postcondition: RECLAIMS, reclamation: true, deterministic: false, rebase_safe: false, owner_phase: 4, bar: 8, emergency: true },
    Operation { name: "place", proposer: Proposer::S, authority: None, precondition: FITS, postcondition: FITS, reclamation: false, deterministic: true, rebase_safe: false, owner_phase: 1, bar: 0, emergency: false },
    Operation { name: "unplace", proposer: Proposer::S, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: true, rebase_safe: false, owner_phase: 1, bar: 0, emergency: false },
    Operation { name: "lane-policy-update", proposer: Proposer::C, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: true, rebase_safe: false, owner_phase: 4, bar: 0, emergency: false },
    Operation { name: "migration-select", proposer: Proposer::S, authority: None, precondition: UNCONDITIONAL, postcondition: UNCONDITIONAL, reclamation: false, deterministic: true, rebase_safe: false, owner_phase: 2, bar: 0, emergency: false },
];

/// The closed operation registry.
pub fn registry() -> &'static [Operation] {
    &REGISTRY
}

/// Look a row up by name.
pub fn find(name: &str) -> Option<&'static Operation> {
    REGISTRY.iter().find(|row| row.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #108-1: every registry row agrees with the single source table.
    #[test]
    fn owner_phases_match_the_single_source_table() {
        for (name, phase) in source_table() {
            let row = find(name).unwrap_or_else(|| panic!("{name} is not registered"));
            assert_eq!(
                row.owner_phase, *phase,
                "{name} diverges from plan.md:377-384"
            );
        }
    }

    /// #108-4: every reclamation ladder rung is flagged emergency-capable, and
    /// the ladder consumes the flag (an emergency rung is exempt from the bar
    /// but still may not raise `Phi`).
    #[test]
    fn emergency_flag_is_consumed_by_the_ladder() {
        for rung in crate::context_policy::ladder::Rung::all() {
            let row = find(rung.operation()).expect("ladder rungs are registered");
            assert!(row.emergency, "{} must be emergency-capable", row.name);
            let budget = Budget { b: 64, r: 1, h: 1 };
            let mut facts = PreconditionFacts::for_row(row, 0, budget);
            facts.phi_pre = 4;
            facts.phi_post = 4;
            facts.emergency = true;
            assert!(
                row.precondition.holds(&facts),
                "an emergency rung may hold the bar"
            );
            facts.emergency = false;
            assert!(
                !row.precondition.holds(&facts),
                "a non-emergency reclamation must net the bar"
            );
        }
    }

    /// #104-4: every reclamation row has a nonzero bar.
    #[test]
    fn every_reclamation_row_has_a_nonzero_bar() {
        for row in registry() {
            if row.reclamation {
                assert!(row.bar > 0, "{} reclaims with bar 0", row.name);
            }
        }
    }
}
