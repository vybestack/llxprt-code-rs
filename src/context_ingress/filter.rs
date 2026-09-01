//! Versioned pre-entry filter registry (outside the ingress transaction).
//!
//! The filter is rule-based, per tool, and sees only evidential items. It digests
//! candidate content into three classes: non-droppable exact spans, ranked compressible
//! content, and bulk noise. A size floor passes verbatim; unusual unknown spans below a
//! bound route verbatim; every digest carries the store raw handle. Rule and
//! preservation-vocabulary versions are stable; in-session updates are relaxation-only,
//! and a tightening request is an explicit rejected mode rather than a silent change.

use crate::context_ingress::segment::Segment;
use std::ops::Range;

/// Filter content class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterClass {
    Exact,
    Ranked,
    Noise,
}

impl FilterClass {
    /// Stable name for reports.
    pub fn name(self) -> &'static str {
        match self {
            FilterClass::Exact => "exact",
            FilterClass::Ranked => "ranked",
            FilterClass::Noise => "noise",
        }
    }
}

/// One preserved span inside a digest, with its vocabulary label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreservedSpan {
    pub span: Range<usize>,
    pub label: &'static str,
}

/// One digest produced by the filter for one candidate payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Digest {
    /// Store raw handle the content came from.
    pub handle: String,
    /// Store ranges this digest covers.
    pub ranges: Vec<Range<u64>>,
    pub class: FilterClass,
    /// Preserved labeled spans (recall evidence).
    pub preserved: Vec<PreservedSpan>,
    /// Digest bytes carried forward as the handle's summary.
    pub summary: Vec<u8>,
    /// Rule version used to build this digest.
    pub rule_version: u64,
    /// Preservation vocabulary version used to build this digest.
    pub vocabulary_version: u64,
}

/// Rule outcome for one candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleVerdict {
    PassVerbatim,
    Digest,
    DropBulk,
}

/// Versioned rule set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterRules {
    pub version: u64,
    /// Payloads at or above this size pass verbatim.
    pub size_floor: usize,
    /// Unknown-shaped spans shorter than this route verbatim.
    pub unknown_bound: usize,
    /// Tools whose output is never filtered.
    pub verbatim_tools: Vec<String>,
}

impl FilterRules {
    /// Baseline version 1 rules.
    pub fn v1() -> Self {
        Self {
            version: 1,
            size_floor: 1024,
            unknown_bound: 64,
            verbatim_tools: Vec::new(),
        }
    }

    /// Whether `update` is a legal relaxation of these rules.
    pub fn is_relaxation_of(&self, update: &FilterRules) -> bool {
        update.size_floor >= self.size_floor
            && update.unknown_bound >= self.unknown_bound
            && update
                .verbatim_tools
                .iter()
                .all(|tool| self.verbatim_tools.contains(tool))
    }
}

/// Versioned preservation vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vocabulary {
    pub version: u64,
    /// Labels preserved by name in every digest.
    pub labels: Vec<&'static str>,
}

impl Vocabulary {
    /// Baseline version 1 vocabulary.
    pub fn v1() -> Self {
        Self {
            version: 1,
            labels: vec!["error-span", "identifier"],
        }
    }
}

/// An update the registry refused to apply in-session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RejectedUpdate {
    /// Tightening requires the offline channel; named explicitly, never silent.
    TighteningRequiresOffline { from: u64, to: u64 },
}

impl RejectedUpdate {
    /// Stable name for reports.
    pub fn name(&self) -> &'static str {
        "tightening-requires-offline"
    }
}

/// Versioned filter registry with relaxation-only in-session updates.
pub struct FilterRegistry {
    rules: Vec<FilterRules>,
    vocabulary: Vec<Vocabulary>,
}

impl FilterRegistry {
    /// Creates a registry seeded with the baseline versions.
    pub fn new() -> Self {
        Self {
            rules: vec![FilterRules::v1()],
            vocabulary: vec![Vocabulary::v1()],
        }
    }

    /// Current rule version.
    pub fn rules(&self) -> &FilterRules {
        self.rules.last().expect("registry seeded with v1")
    }

    /// Current vocabulary version.
    pub fn vocabulary(&self) -> &Vocabulary {
        self.vocabulary.last().expect("registry seeded with v1")
    }

    /// Resolves the rule set for a historical version for the session's life.
    pub fn rules_at(&self, version: u64) -> Option<&FilterRules> {
        self.rules.iter().find(|rules| rules.version == version)
    }

    /// Resolves a historical vocabulary version.
    pub fn vocabulary_at(&self, version: u64) -> Option<&Vocabulary> {
        self.vocabulary
            .iter()
            .find(|vocabulary| vocabulary.version == version)
    }

    /// Applies an in-session rule update; only relaxations are accepted.
    pub fn update_rules(&mut self, update: FilterRules) -> Result<u64, RejectedUpdate> {
        let current = self.rules();
        if update.version <= current.version {
            return Ok(current.version);
        }
        if !current.is_relaxation_of(&update) {
            return Err(RejectedUpdate::TighteningRequiresOffline {
                from: current.version,
                to: update.version,
            });
        }
        self.rules.push(update);
        Ok(self.rules().version)
    }

    /// Applies an in-session vocabulary update; additions only.
    pub fn update_vocabulary(&mut self, update: Vocabulary) -> Result<u64, RejectedUpdate> {
        let current = self.vocabulary();
        if update.version <= current.version {
            return Ok(current.version);
        }
        if !update
            .labels
            .iter()
            .all(|label| current.labels.contains(label))
        {
            return Err(RejectedUpdate::TighteningRequiresOffline {
                from: current.version,
                to: update.version,
            });
        }
        self.vocabulary.push(update);
        Ok(self.vocabulary().version)
    }

    /// Rule verdict for one candidate payload from one tool.
    pub fn verdict(&self, tool: &str, segments: &[Segment], total: usize) -> RuleVerdict {
        let rules = self.rules();
        if rules.verbatim_tools.iter().any(|name| name == tool) {
            return RuleVerdict::PassVerbatim;
        }
        if total >= rules.size_floor {
            return RuleVerdict::PassVerbatim;
        }
        let has_exact = segments.iter().any(|segment| {
            matches!(
                segment.class,
                crate::context_ingress::segment::StructuralClass::ExactSpan
                    | crate::context_ingress::segment::StructuralClass::Identifier
            )
        });
        if has_exact {
            return RuleVerdict::Digest;
        }
        if total < rules.unknown_bound {
            return RuleVerdict::PassVerbatim;
        }
        RuleVerdict::DropBulk
    }

    /// Builds a digest for one candidate, preserving labeled spans for recall.
    pub fn digest(
        &self,
        tool: &str,
        handle: &str,
        ranges: Vec<Range<u64>>,
        bytes: &[u8],
        segments: &[Segment],
    ) -> Digest {
        let class = if self.verdict(tool, segments, bytes.len()) == RuleVerdict::DropBulk
            && self.verdict(tool, segments, bytes.len()) != RuleVerdict::Digest
        {
            FilterClass::Noise
        } else if self.verdict(tool, segments, bytes.len()) == RuleVerdict::Digest {
            FilterClass::Exact
        } else {
            FilterClass::Ranked
        };
        let vocabulary = self.vocabulary();
        let mut preserved = Vec::new();
        let mut summary = Vec::new();
        for segment in segments {
            let label = match segment.class {
                crate::context_ingress::segment::StructuralClass::ExactSpan => Some("error-span"),
                crate::context_ingress::segment::StructuralClass::Identifier => Some("identifier"),
                _ => None,
            };
            if let Some(label) = label {
                if vocabulary.labels.contains(&label) {
                    preserved.push(PreservedSpan {
                        span: segment.span.clone(),
                        label,
                    });
                    summary.extend_from_slice(&bytes[segment.span.clone()]);
                }
            }
        }
        Digest {
            handle: handle.to_string(),
            ranges,
            class,
            preserved,
            summary,
            rule_version: self.rules().version,
            vocabulary_version: vocabulary.version,
        }
    }
}

impl Default for FilterRegistry {
    fn default() -> Self {
        Self::new()
    }
}
