//! Closed operation registry (design.tex §9 `tab:ops`).
//!
//! One row per operation. Columns map onto [`Operation`] fields: proposer,
//! reclamation class (`reclamation`), deterministic, rebase-safe, owner phase.
//! Rows whose owner phase is not 3 are still registered; the executor answers
//! them with a `capability_not_landed` result instead of executing them.

/// Who may propose an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// A single registry row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Operation {
    /// Operation name as written in `tab:ops` (kebab-case).
    pub name: &'static str,
    /// Proposing role.
    pub proposer: Proposer,
    /// Secondary authority that must not increase (dual-proposer rows).
    pub authority: Option<Proposer>,
    /// Commit precondition (design §8.3).
    pub precondition: &'static str,
    /// Postcondition observable after commit.
    pub postcondition: &'static str,
    /// Reclamation class: net `Phi` must drop by at least the bar.
    pub reclamation: bool,
    /// Deterministic operation.
    pub deterministic: bool,
    /// Rebase-safe: re-applies on the actual parent instead of aborting.
    pub rebase_safe: bool,
    /// Phase that owns the implementation of this row.
    pub owner_phase: u8,
}

#[rustfmt::skip]
static REGISTRY: [Operation; 66] = [
    Operation { name: "admit-ingress", proposer: Proposer::S, authority: None, precondition: "sanitized payload fits scope", postcondition: "payload appended in scope", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 2 },
    Operation { name: "admit-observation", proposer: Proposer::S, authority: None, precondition: "observation keyed, scope writable", postcondition: "observation recorded", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 2 },
    Operation { name: "admit-as-handle", proposer: Proposer::S, authority: None, precondition: "handle resolves to live item", postcondition: "handle recorded as item", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 2 },
    Operation { name: "admit-feedback", proposer: Proposer::S, authority: None, precondition: "feedback within vocabulary", postcondition: "feedback stored in scope", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 3 },
    Operation { name: "pending-response-stage", proposer: Proposer::S, authority: None, precondition: "provider turn pending", postcondition: "response staged in lane", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 3 },
    Operation { name: "admit-response", proposer: Proposer::S, authority: None, precondition: "staged response sanitized", postcondition: "response appended to scope", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 3 },
    Operation { name: "admit-obligation", proposer: Proposer::S, authority: None, precondition: "obligation ledger open", postcondition: "obligation recorded on lane", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 5 },
    Operation { name: "admit-convenience", proposer: Proposer::S, authority: None, precondition: "convenience within budget", postcondition: "convenience item stored", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 5 },
    Operation { name: "sanitize", proposer: Proposer::S, authority: None, precondition: "raw payload present", postcondition: "sanitized payload emitted", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 2 },
    Operation { name: "redact", proposer: Proposer::O, authority: None, precondition: "redaction rule present", postcondition: "redacted bytes durable", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 8 },
    Operation { name: "note", proposer: Proposer::M, authority: Some(Proposer::C), precondition: "item writable, fits bound", postcondition: "note attached to item", reclamation: false, deterministic: false, rebase_safe: true, owner_phase: 3 },
    Operation { name: "read-back", proposer: Proposer::M, authority: None, precondition: "item within read-back window", postcondition: "read-back recorded", reclamation: false, deterministic: false, rebase_safe: true, owner_phase: 3 },
    Operation { name: "re-promote", proposer: Proposer::C, authority: None, precondition: "item demoted and repairable", postcondition: "item restored to region", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 4 },
    Operation { name: "placeholder-collapse", proposer: Proposer::C, authority: None, precondition: "placeholder resolvable", postcondition: "placeholder replaced, Phi drops", reclamation: true, deterministic: false, rebase_safe: false, owner_phase: 3 },
    Operation { name: "drop-with-handle", proposer: Proposer::C, authority: None, precondition: "handle registered", postcondition: "item dropped, Phi drops", reclamation: true, deterministic: true, rebase_safe: false, owner_phase: 3 },
    Operation { name: "fold-away-ephemeral", proposer: Proposer::C, authority: None, precondition: "ephemeral siblings adjacent", postcondition: "ephemeral folded away", reclamation: true, deterministic: false, rebase_safe: false, owner_phase: 3 },
    Operation { name: "condense", proposer: Proposer::C, authority: None, precondition: "item condensable", postcondition: "item condensed, Phi drops", reclamation: true, deterministic: false, rebase_safe: false, owner_phase: 4 },
    Operation { name: "fold", proposer: Proposer::C, authority: None, precondition: "fold target writable", postcondition: "folds applied, Phi drops", reclamation: true, deterministic: false, rebase_safe: false, owner_phase: 4 },
    Operation { name: "compact", proposer: Proposer::C, authority: None, precondition: "region over floor", postcondition: "region compacted, Phi drops", reclamation: true, deterministic: false, rebase_safe: false, owner_phase: 4 },
    Operation { name: "regenerate", proposer: Proposer::C, authority: None, precondition: "regeneration prompt durable", postcondition: "items regenerated, Phi drops", reclamation: true, deterministic: false, rebase_safe: false, owner_phase: 4 },
    Operation { name: "pin-override-collapse", proposer: Proposer::C, authority: None, precondition: "pin override asserted", postcondition: "override collapsed, Phi drops", reclamation: true, deterministic: false, rebase_safe: false, owner_phase: 3 },
    Operation { name: "demote", proposer: Proposer::L, authority: None, precondition: "lane item demotable", postcondition: "lane ledger demotes item", reclamation: true, deterministic: false, rebase_safe: false, owner_phase: 5 },
    Operation { name: "discharge", proposer: Proposer::L, authority: None, precondition: "obligation satisfied", postcondition: "obligation discharged", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 5 },
    Operation { name: "consolidate-obligations", proposer: Proposer::L, authority: None, precondition: "obligations co-referring", postcondition: "obligations merged, Phi drops", reclamation: true, deterministic: false, rebase_safe: false, owner_phase: 5 },
    Operation { name: "escalate-pending", proposer: Proposer::O, authority: None, precondition: "escalation threshold met", postcondition: "pending item escalated", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 4 },
    Operation { name: "escalate-complete", proposer: Proposer::O, authority: None, precondition: "escalation complete", postcondition: "completion recorded", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 4 },
    Operation { name: "amend", proposer: Proposer::M, authority: None, precondition: "amend within authority", postcondition: "item amended", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 6 },
    Operation { name: "retract", proposer: Proposer::M, authority: None, precondition: "retract within authority", postcondition: "item retracted", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 6 },
    Operation { name: "promote", proposer: Proposer::M, authority: None, precondition: "item promotable", postcondition: "item promoted", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 6 },
    Operation { name: "reclassify", proposer: Proposer::M, authority: None, precondition: "class legal for item", postcondition: "item reclassified", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 6 },
    Operation { name: "resegment", proposer: Proposer::M, authority: Some(Proposer::C), precondition: "segment over threshold", postcondition: "children claim-atomic", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 3 },
    Operation { name: "reopen", proposer: Proposer::M, authority: None, precondition: "scope closed, authority holds", postcondition: "scope reopened", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 7 },
    Operation { name: "pin", proposer: Proposer::M, authority: None, precondition: "item pinnable", postcondition: "pin registered", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 3 },
    Operation { name: "unpin", proposer: Proposer::M, authority: None, precondition: "pin held", postcondition: "pin released", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 3 },
    Operation { name: "expire-pin", proposer: Proposer::S, authority: None, precondition: "pin lease elapsed", postcondition: "pin expired", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 3 },
    Operation { name: "declare-boundary", proposer: Proposer::C, authority: None, precondition: "boundary declared by C", postcondition: "scope closed by declaration", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 4 },
    Operation { name: "scope-open", proposer: Proposer::S, authority: None, precondition: "parent scope open", postcondition: "child scope open", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 1 },
    Operation { name: "scope-close", proposer: Proposer::S, authority: None, precondition: "scope quiescent", postcondition: "scope closed", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 1 },
    Operation { name: "decay", proposer: Proposer::S, authority: None, precondition: "decay schedule due", postcondition: "weights decayed", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 5 },
    Operation { name: "flush-notes", proposer: Proposer::S, authority: Some(Proposer::C), precondition: "notes present", postcondition: "notes flushed to ledger", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 4 },
    Operation { name: "arm", proposer: Proposer::S, authority: None, precondition: "trigger idle", postcondition: "trigger armed", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 4 },
    Operation { name: "disarm", proposer: Proposer::S, authority: None, precondition: "trigger armed", postcondition: "trigger disarmed", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 4 },
    Operation { name: "rule-update", proposer: Proposer::M, authority: Some(Proposer::C), precondition: "rule version signed", postcondition: "rule version active", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 4 },
    Operation { name: "vocabulary-update", proposer: Proposer::M, authority: Some(Proposer::C), precondition: "vocabulary version signed", postcondition: "vocabulary active", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 4 },
    Operation { name: "fact-invalidate", proposer: Proposer::S, authority: None, precondition: "fact contradicted", postcondition: "fact invalidated", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 5 },
    Operation { name: "revalidate", proposer: Proposer::L, authority: None, precondition: "lane sweep pending", postcondition: "lane revalidated", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 5 },
    Operation { name: "stale-demote", proposer: Proposer::S, authority: None, precondition: "stale threshold met", postcondition: "stale item demoted", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 5 },
    Operation { name: "repair-result", proposer: Proposer::S, authority: None, precondition: "repair applied", postcondition: "repair result recorded", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 3 },
    Operation { name: "branch-open", proposer: Proposer::M, authority: None, precondition: "branch point durable", postcondition: "branch opened", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 8 },
    Operation { name: "branch-return", proposer: Proposer::M, authority: None, precondition: "branch open", postcondition: "branch returned", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 8 },
    Operation { name: "branch-abort", proposer: Proposer::M, authority: Some(Proposer::C), precondition: "branch open", postcondition: "branch aborted", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 8 },
    Operation { name: "checkpoint", proposer: Proposer::S, authority: None, precondition: "store quiescent", postcondition: "checkpoint durable", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 2 },
    Operation { name: "lease-acquire", proposer: Proposer::S, authority: None, precondition: "lease free, epoch higher", postcondition: "writer fenced by epoch", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 3 },
    Operation { name: "render-contract-observed", proposer: Proposer::S, authority: None, precondition: "provider turn observed", postcondition: "contract observation logged", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 7 },
    Operation { name: "calibration-update", proposer: Proposer::S, authority: None, precondition: "calibration sample", postcondition: "calibration updated", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 9 },
    Operation { name: "margin-recalibrate", proposer: Proposer::S, authority: None, precondition: "margin drift detected", postcondition: "margin recalibrated", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 9 },
    Operation { name: "store-mode", proposer: Proposer::S, authority: None, precondition: "mode legal", postcondition: "store mode active", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 2 },
    Operation { name: "queue-service", proposer: Proposer::S, authority: None, precondition: "queue non-empty", postcondition: "queue serviced", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 4 },
    Operation { name: "spend-account", proposer: Proposer::S, authority: None, precondition: "spend authorized", postcondition: "spend accounted", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 3 },
    Operation { name: "journal-append", proposer: Proposer::S, authority: None, precondition: "entry sealed", postcondition: "journal appended", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 3 },
    Operation { name: "wrap-up", proposer: Proposer::C, authority: None, precondition: "turn complete", postcondition: "turn wrapped up", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 4 },
    Operation { name: "quiesce-unwritable", proposer: Proposer::S, authority: None, precondition: "region unwritable", postcondition: "region quiesced", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 2 },
    Operation { name: "index-rebuild", proposer: Proposer::S, authority: Some(Proposer::O), precondition: "index dirty", postcondition: "index rebuilt", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 9 },
    Operation { name: "import", proposer: Proposer::O, authority: None, precondition: "import sanitized", postcondition: "import applied", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 8 },
    Operation { name: "consolidate-head", proposer: Proposer::O, authority: None, precondition: "heads adjacent", postcondition: "heads consolidated", reclamation: false, deterministic: false, rebase_safe: false, owner_phase: 4 },
    Operation { name: "consolidate-notes", proposer: Proposer::C, authority: None, precondition: "notes co-referring", postcondition: "notes merged, Phi drops", reclamation: true, deterministic: false, rebase_safe: false, owner_phase: 4 },
];

/// The closed operation registry.
pub fn registry() -> &'static [Operation] {
    &REGISTRY
}

/// Look a row up by name.
pub fn find(name: &str) -> Option<&'static Operation> {
    REGISTRY.iter().find(|row| row.name == name)
}
