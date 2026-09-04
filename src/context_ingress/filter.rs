//! Versioned pre-entry filter registry (outside the ingress transaction).
//!
//! The filter is rule-based, per tool, and sees only evidential items. It digests
//! candidate content into three classes: non-droppable exact spans, ranked compressible
//! content, and bulk noise. A size floor passes verbatim; unusual unknown spans below a
//! bound route verbatim; every digest carries the store raw handle. Rule and
//! preservation-vocabulary versions are stable; in-session updates are relaxation-only,
//! and a tightening request is an explicit rejected mode rather than a silent change.

use crate::context_ingress::segment::Segment;
use serde::{Deserialize, Serialize};
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterRules {
    pub version: u64,
    /// Bulk-evidence admission floor: payloads at or above this size are
    /// digested into a bounded handle (issue #119).
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
            && self
                .verbatim_tools
                .iter()
                .all(|tool| update.verbatim_tools.contains(tool))
    }
}

/// Versioned preservation vocabulary.
#[derive(Clone, PartialEq, Eq)]
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
        if !current
            .labels
            .iter()
            .all(|label| update.labels.contains(label))
        {
            return Err(RejectedUpdate::TighteningRequiresOffline {
                from: current.version,
                to: update.version,
            });
        }
        self.vocabulary.push(update);
        Ok(self.vocabulary().version)
    }

    /// Durable, serializable form of one vocabulary version.
    pub fn vocabulary_snapshots(&self) -> Vec<VocabularySnapshot> {
        self.vocabulary
            .iter()
            .map(|vocabulary| VocabularySnapshot {
                version: vocabulary.version,
                labels: vocabulary
                    .labels
                    .iter()
                    .map(|label| label.to_string())
                    .collect(),
            })
            .collect()
    }

    /// Restores the vocabulary history from its durable form. Additions only:
    /// dropping a label is a typed refusal (issue #118).
    pub fn restore_vocabulary_snapshots(
        &mut self,
        snapshots: Vec<VocabularySnapshot>,
    ) -> Result<(), RejectedUpdate> {
        let mut restored = Vec::with_capacity(snapshots.len());
        let mut labels = Vocabulary::v1().labels;
        for (index, snapshot) in snapshots.iter().enumerate() {
            if index as u64 + 1 != snapshot.version
                || !labels
                    .iter()
                    .all(|label| snapshot.labels.contains(&label.to_string()))
            {
                return Err(RejectedUpdate::TighteningRequiresOffline {
                    from: index as u64 + 1,
                    to: snapshot.version,
                });
            }
            restored.push(Vocabulary {
                version: snapshot.version,
                labels: std::mem::take(&mut labels),
            });
            labels = snapshot
                .labels
                .iter()
                .map(|l| Box::leak(l.as_str().to_string().into_boxed_str()) as &'static str)
                .collect();
        }
        restored.push(Vocabulary {
            version: snapshots.len() as u64 + 1,
            labels,
        });
        self.vocabulary = restored;
        Ok(())
    }

    /// Durable history of every rule version this session adopted, oldest first.
    /// Persisted after the run and reloaded after a restart so a historical
    /// rule version keeps resolving (issue #118).
    pub fn rules_history(&self) -> &[FilterRules] {
        &self.rules
    }

    /// Durable history of every vocabulary version this session adopted.
    pub fn vocabulary_history(&self) -> &[Vocabulary] {
        &self.vocabulary
    }

    /// Restores the versioned histories from a durable artifact. Each history
    /// must be non-empty, begin at version 1, and advance by strictly
    /// increasing versions that are legal relaxations of their predecessor;
    /// anything else is a typed refusal instead of a silent rewrite (issue
    /// #118).
    pub fn restore_histories(&mut self, rules: Vec<FilterRules>) -> Result<(), RejectedUpdate> {
        if rules.first().map(|rules| rules.version) != Some(1) {
            return Err(RejectedUpdate::TighteningRequiresOffline {
                from: 1,
                to: rules.first().map(|rules| rules.version).unwrap_or(0),
            });
        }
        let mut current = rules[0].clone();
        for update in rules.iter().skip(1) {
            if update.version <= current.version || !current.is_relaxation_of(update) {
                return Err(RejectedUpdate::TighteningRequiresOffline {
                    from: current.version,
                    to: update.version,
                });
            }
            current = update.clone();
        }
        self.rules = rules;
        Ok(())
    }

    /// Rule verdict for one candidate payload from one tool.
    ///
    /// The size floor is an *admission* floor for bulk evidence: a payload at or
    /// above it is bulk evidence that must be digested into a bounded handle
    /// (issue #119), never passed verbatim into the request list. Verbatim
    /// routing is reserved for the tool list and for unknown-shaped spans below
    /// the unknown bound.
    pub fn verdict(&self, tool: &str, segments: &[Segment], total: usize) -> RuleVerdict {
        let rules = self.rules();
        if rules.verbatim_tools.iter().any(|name| name == tool) {
            return RuleVerdict::PassVerbatim;
        }
        if total >= rules.size_floor {
            return RuleVerdict::Digest;
        }
        use crate::context_ingress::segment::StructuralClass;
        let has_exact = segments.iter().any(|segment| {
            matches!(
                segment.class,
                StructuralClass::ExactSpan | StructuralClass::Identifier
            )
        });
        if has_exact {
            return RuleVerdict::Digest;
        }
        // Ranked, compressible content is kept in ranked form rather than dropped.
        let has_ranked = segments.iter().any(|segment| {
            matches!(
                segment.class,
                StructuralClass::Code | StructuralClass::TestLog
            )
        });
        if has_ranked {
            return RuleVerdict::Digest;
        }
        // Recognized bulk noise is droppable at any size.
        let has_noise = segments
            .iter()
            .any(|segment| matches!(segment.class, StructuralClass::Noise));
        if has_noise {
            return RuleVerdict::DropBulk;
        }
        // Unusual, unknown-shaped spans below the bound route verbatim: fail safe.
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
        let class = match self.verdict(tool, segments, bytes.len()) {
            RuleVerdict::Digest => FilterClass::Exact,
            RuleVerdict::DropBulk => FilterClass::Noise,
            RuleVerdict::PassVerbatim => FilterClass::Ranked,
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

/// Durable form of one preservation vocabulary version (issue #118).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VocabularySnapshot {
    pub version: u64,
    pub labels: Vec<String>,
}
