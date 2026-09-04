//! Context-store migration framing: crash matrix and publication machinery.
//!
//! The crash matrix is the whole decision: after a crash the store either keeps the
//! complete v2 store or selects the complete v3 store. There is no partially
//! migrated store, because selection is an event in the log and v3 is only selected
//! once the private build is complete.

use crate::context_kernel::canonical::{digest, Digest, HashScope, Sink};
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

/// One store generation, typed by how it is identified. The two generations of a
/// migration live in different hash scopes: a committed generation is identified
/// by the chain over its recorded events, and a private build is identified by a
/// checksum over its bytes, computed inside [`HashScope::StoreBuild`]. A value
/// from one scope never verifies in the other, so corruption in one generation
/// cannot invalidate the other's identity.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Generation {
    /// The committed generation readers resolve.
    Committed {
        /// Context-store version of the generation.
        store_version: u64,
        /// Bytes the generation covers.
        bytes: u64,
        /// Chain value over the generation's recorded events.
        chain: Digest,
    },
    /// A completed private build, invisible to readers until the swap.
    Built {
        /// Context-store version the build targets.
        store_version: u64,
        /// Bytes the build covers.
        bytes: u64,
        /// Checksum over the built bytes, inside the store-build scope.
        checksum: Digest,
    },
}

impl Generation {
    /// Context-store version of the generation.
    pub fn store_version(self) -> u64 {
        match self {
            Generation::Committed { store_version, .. } => store_version,
            Generation::Built { store_version, .. } => store_version,
        }
    }

    /// Bytes the generation covers.
    pub fn bytes(self) -> u64 {
        match self {
            Generation::Committed { bytes, .. } | Generation::Built { bytes, .. } => bytes,
        }
    }

    /// Hash scope the generation's identity lives in.
    pub fn scope(self) -> HashScope {
        match self {
            Generation::Committed { .. } => HashScope::EventChain,
            Generation::Built { .. } => HashScope::StoreBuild,
        }
    }
}

/// Which failure a publication step refuses with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PublicationError {
    /// The inactive slot held no completed build to swap in.
    NoBuildPending,
    /// The inactive slot already held a completed build.
    BuildPending,
    /// A generation was offered to a slot whose contract it does not satisfy.
    SlotContract {
        /// Scope the slot requires.
        expected: HashScope,
        /// Scope the offered generation carries.
        found: HashScope,
    },
    /// The publication already committed; a publication happens at most once.
    AlreadyPublished,
}

/// The two storage slots of a migration. Exactly one slot is active; a build is
/// written into the inactive slot and an explicit swap moves visibility, so a
/// crash between the write and the swap leaves the committed generation active.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SlotPair {
    active: Generation,
    inactive: Option<Generation>,
    published: bool,
}

impl SlotPair {
    /// Frames the slot pair of an unmigrated store: the committed generation is
    /// active and the inactive slot is free.
    pub fn genesis(store_version: u64, bytes: u64, chain: Digest) -> Self {
        Self {
            active: Generation::Committed {
                store_version,
                bytes,
                chain,
            },
            inactive: None,
            published: false,
        }
    }

    /// The generation readers resolve.
    pub fn active(&self) -> &Generation {
        &self.active
    }

    /// A landed build awaiting the swap, if any.
    pub fn inactive(&self) -> Option<&Generation> {
        self.inactive.as_ref()
    }

    /// Whether the swap already happened.
    pub fn published(&self) -> bool {
        self.published
    }

    /// Writes a completed build into the inactive slot. Visibility is untouched:
    /// a crash here leaves the committed generation active. The slot accepts only
    /// a build whose identity is inside the store-build scope.
    pub fn land(&mut self, build: Generation) -> Result<(), PublicationError> {
        if self.inactive.is_some() {
            return Err(PublicationError::BuildPending);
        }
        if build.scope() != HashScope::StoreBuild {
            return Err(PublicationError::SlotContract {
                expected: HashScope::StoreBuild,
                found: build.scope(),
            });
        }
        self.inactive = Some(build);
        Ok(())
    }

    /// Discards a landed build without swapping, so a failed qualification can be
    /// retried; the committed generation is untouched.
    pub fn discard(&mut self) -> Result<(), PublicationError> {
        if self.inactive.is_none() {
            return Err(PublicationError::NoBuildPending);
        }
        self.inactive = None;
        Ok(())
    }

    /// The atomic visibility transition: the landed build becomes the committed
    /// generation, carrying the selection event's chain value, and the retired
    /// generation moves to the inactive slot. A crash before this call leaves the
    /// old generation active; the call itself happens at most once.
    pub fn swap(&mut self, selection_chain: Digest) -> Result<Generation, PublicationError> {
        if self.published {
            return Err(PublicationError::AlreadyPublished);
        }
        let build = match self.inactive.take() {
            Some(build) => build,
            None => return Err(PublicationError::NoBuildPending),
        };
        let retired = std::mem::replace(
            &mut self.active,
            Generation::Committed {
                store_version: build.store_version(),
                bytes: build.bytes(),
                chain: selection_chain,
            },
        );
        self.inactive = Some(retired);
        self.published = true;
        Ok(build)
    }
}

/// Durable record of a completed publication. The two identities it carries are
/// computed in different hash scopes: the build checksum identifies the published
/// bytes inside the store-build scope, and the selection chain is the event-chain
/// value the committed selection event continues. Recovery verifies each field in
/// its own scope, so a corrupted field refuses verification without invalidating
/// the other scope's evidence.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MigrationDescriptor {
    /// Store version the publication switched readers to.
    pub store_version: u64,
    /// Checksum over the published bytes, inside the store-build scope.
    pub build_checksum: Digest,
    /// Event-chain value the selection event commits to.
    pub selection_chain: Digest,
    /// Whether the publication committed.
    pub published: bool,
}

impl MigrationDescriptor {
    /// Seals the descriptor of a completed publication.
    pub fn seal(store_version: u64, build_checksum: Digest, selection_chain: Digest) -> Self {
        Self {
            store_version,
            build_checksum,
            selection_chain,
            published: true,
        }
    }

    /// Whether `bytes` is the published generation, verified inside the
    /// store-build scope.
    pub fn verify_build(&self, bytes: &[u8]) -> bool {
        HashScope::StoreBuild.digest(bytes) == self.build_checksum
    }

    /// Whether `log` is the log the selection committed, verified inside the
    /// event-chain scope.
    pub fn verify_chain(&self, log: &EventLog) -> bool {
        log.head_checksum() == self.selection_chain
    }

    /// Encodes the descriptor into `sink`, with a byte-order mark between the
    /// header and the two scoped values.
    pub fn encode(&self, sink: &mut Sink) {
        sink.tag("migration-descriptor");
        sink.int(self.store_version);
        sink.byte_order_mark();
        sink.int(self.build_checksum);
        sink.int(self.selection_chain);
        sink.flag(self.published);
    }
}
