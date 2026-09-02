//! Phase 2 operation-registry rows (issue #39): types only, executor lands in Phase 3.
//!
//! The rows this phase owns are `admit-ingress`, `sanitize`, `redact`, `import`,
//! `rule-update`, `vocabulary-update`, `index-rebuild`, `store-mode`, and the store side
//! of `quiesce-unwritable`. They are additive to the Phase 1 registry: no existing row or
//! event kind changes shape.

/// Phase 2 owned operation rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreOperation {
    AdmitIngress,
    Sanitize,
    Redact,
    Import,
    RuleUpdate,
    VocabularyUpdate,
    IndexRebuild,
    StoreMode,
    QuiesceUnwritable,
}

impl StoreOperation {
    /// Stable operation-table name.
    pub fn name(self) -> &'static str {
        match self {
            StoreOperation::AdmitIngress => "admit-ingress",
            StoreOperation::Sanitize => "sanitize",
            StoreOperation::Redact => "redact",
            StoreOperation::Import => "import",
            StoreOperation::RuleUpdate => "rule-update",
            StoreOperation::VocabularyUpdate => "vocabulary-update",
            StoreOperation::IndexRebuild => "index-rebuild",
            StoreOperation::StoreMode => "store-mode",
            StoreOperation::QuiesceUnwritable => "quiesce-unwritable",
        }
    }

    /// Every Phase 2 row, in registry order.
    pub fn all() -> [StoreOperation; 9] {
        [
            StoreOperation::AdmitIngress,
            StoreOperation::Sanitize,
            StoreOperation::Redact,
            StoreOperation::Import,
            StoreOperation::RuleUpdate,
            StoreOperation::VocabularyUpdate,
            StoreOperation::IndexRebuild,
            StoreOperation::StoreMode,
            StoreOperation::QuiesceUnwritable,
        ]
    }

    /// Whether the row commits new governed state (and so needs a writable store).
    pub fn advances_state(self) -> bool {
        !matches!(self, StoreOperation::QuiesceUnwritable)
    }
}
