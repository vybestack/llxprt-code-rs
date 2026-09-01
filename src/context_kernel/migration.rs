//! Context-store migration framing: crash matrix and publication machinery.
//!
//! The crash matrix is the whole decision: after a crash the store either keeps the
//! complete v2 store or selects the complete v3 store. There is no partially
//! migrated store, because selection is an event in the log and v3 is only selected
//! once the private build is complete.

use crate::context_kernel::canonical::{digest, Digest, Sink};
use crate::context_kernel::events::{EventKind, EventLog, OperationClass};
use crate::context_kernel::ir::StoreRange;

/// Context-store version of the pre-migration store.
pub const V2: u64 = 2;
/// Context-store version of the migrated store.
pub const V3: u64 = 3;

/// Outcome of the crash matrix applied to a recovered log.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum MigrationDecision {
    /// Keep the complete v2 store.
    KeepV2 {
        /// Why v2 was kept.
        reason: String,
    },
    /// Select the complete v3 store.
    SelectV3 {
        /// Log sequence that carried the selection event.
        selected_sequence: u64,
    },
}

impl MigrationDecision {
    /// Store version the decision resolves to.
    pub fn store_version(&self) -> u64 {
        match self {
            MigrationDecision::KeepV2 { .. } => V2,
            MigrationDecision::SelectV3 { .. } => V3,
        }
    }

    /// Encodes the decision into `sink`.
    pub fn encode(&self, sink: &mut Sink) {
        match self {
            MigrationDecision::KeepV2 { reason } => {
                sink.tag("keep-v2");
                sink.blob(reason.as_bytes());
            }
            MigrationDecision::SelectV3 { selected_sequence } => {
                sink.tag("select-v3");
                sink.int(*selected_sequence);
            }
        }
    }
}

/// Applies the crash matrix to a recovered log.
///
/// v3 is selected only when the log itself was written under v3, or when a committed
/// migration-select event names v3. Anything else — an empty log, a v2 log, or a log
/// whose selection event names a version other than v3 — keeps the complete v2 store.
pub fn decide(after_crash: &EventLog) -> MigrationDecision {
    if after_crash.store_version() == V3 {
        return MigrationDecision::SelectV3 {
            selected_sequence: selection_sequence(after_crash, V3),
        };
    }
    for event in after_crash.events() {
        if let EventKind::OperationCommit {
            class: OperationClass::MigrationSelect,
            subject,
            ..
        } = &event.kind
        {
            if *subject == V3 {
                return MigrationDecision::SelectV3 {
                    selected_sequence: event.sequence,
                };
            }
            return MigrationDecision::KeepV2 {
                reason: String::from("selection named a version other than v3"),
            };
        }
    }
    MigrationDecision::KeepV2 {
        reason: String::from("no complete v3 selection event in the recovered log"),
    }
}

fn selection_sequence(log: &EventLog, target: u64) -> u64 {
    for event in log.events() {
        if let EventKind::OperationCommit {
            class: OperationClass::MigrationSelect,
            subject,
            ..
        } = &event.kind
        {
            if *subject == target {
                return event.sequence;
            }
        }
    }
    0
}

/// Byte plan for a private build: what the v3 store must cover.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MigrationPlan {
    /// Version the build targets.
    pub target_version: u64,
    /// Store ranges the build copies.
    pub ranges: Vec<StoreRange>,
    /// Checksum of the recovered log the build started from.
    pub source_checksum: Digest,
}

impl MigrationPlan {
    /// Plans a build from `source` to `target`.
    pub fn from(target: u64, ranges: Vec<StoreRange>, checksum: Digest) -> Self {
        Self {
            target_version: target,
            ranges,
            source_checksum: checksum,
        }
    }

    /// Bytes the plan copies, counted once.
    pub fn units(&self) -> u64 {
        crate::context_kernel::ir::covered_units(&self.ranges)
    }

    /// Encodes the plan into `sink`.
    pub fn encode(&self, sink: &mut Sink) {
        sink.tag("migration-plan");
        sink.int(self.target_version);
        sink.int(self.source_checksum);
        for range in &self.ranges {
            sink.int(range.offset);
            sink.int(range.length);
        }
    }
}

/// A private build: invisible to readers until it is complete.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PrivateBuild {
    /// Plan the build follows.
    pub plan: MigrationPlan,
    /// Canonical checksum of the built store, once complete.
    pub built_checksum: Option<Digest>,
    /// Whether the build finished.
    pub complete: bool,
}

impl PrivateBuild {
    /// Starts a build for `plan`.
    pub fn start(plan: MigrationPlan) -> Self {
        Self {
            plan,
            built_checksum: None,
            complete: false,
        }
    }

    /// Marks the build complete with the checksum of the built bytes.
    pub fn complete_with(&mut self, bytes: &[u8]) {
        self.built_checksum = Some(digest(bytes));
        self.complete = true;
    }

    /// Encodes the build into `sink`.
    pub fn encode(&self, sink: &mut Sink) {
        self.plan.encode(sink);
        sink.flag(self.complete);
        match self.built_checksum {
            Some(checksum) => sink.int(checksum),
            None => sink.int(0),
        }
    }
}

/// A publication: one atomic visibility transition, never a partial one.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Publication {
    /// Version the publication switched readers to.
    pub store_version: u64,
    /// Checksum of the private build the publication adopted.
    pub built_checksum: Digest,
    /// Whether the publication happened.
    pub published: bool,
}

impl Publication {
    /// Frames a publication of a completed build.
    pub fn of(build: &PrivateBuild) -> Option<Self> {
        let checksum = build.built_checksum?;
        Some(Self {
            store_version: build.plan.target_version,
            built_checksum: checksum,
            published: false,
        })
    }

    /// Encodes the publication into `sink`.
    pub fn encode(&self, sink: &mut Sink) {
        sink.tag("publication");
        sink.int(self.store_version);
        sink.int(self.built_checksum);
        sink.flag(self.published);
    }
}
