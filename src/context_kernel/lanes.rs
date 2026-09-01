//! Lane classification and the versioned lane-policy registry.

use crate::context_kernel::canonical::Sink;

/// Content class of an item. Lanes partition claims, not messages; one message can
/// carry items in several lanes after claim-atomic splitting.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lane {
    /// System prompt, task statement, explicit constraints, safety rules.
    Constitutional,
    /// Commitments, verified state, open questions, standing decisions.
    Decisional,
    /// Tool outputs, file contents, logs.
    Evidential,
    /// Superseded exploration and failed attempts.
    Ephemeral,
}

impl Lane {
    /// Stable name used in canonical encodings.
    pub fn name(self) -> &'static str {
        match self {
            Lane::Constitutional => "constitutional",
            Lane::Decisional => "decisional",
            Lane::Evidential => "evidential",
            Lane::Ephemeral => "ephemeral",
        }
    }

    /// All lanes in registry order.
    pub fn all() -> [Lane; 4] {
        [
            Lane::Constitutional,
            Lane::Decisional,
            Lane::Evidential,
            Lane::Ephemeral,
        ]
    }
}

/// Target fidelity a lane's items must retain.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fidelity {
    /// Bytes rendered exactly as ingested.
    Verbatim,
    /// Fields preserved, prose compressible.
    FieldPreserving,
    /// Preservation-aware digest with a store handle.
    Digest,
    /// Droppable without a digest.
    Droppable,
}

impl Fidelity {
    /// Stable discriminant used in canonical encodings.
    pub fn code(self) -> u64 {
        match self {
            Fidelity::Verbatim => 1,
            Fidelity::FieldPreserving => 2,
            Fidelity::Digest => 3,
            Fidelity::Droppable => 4,
        }
    }
}

/// Derivative operations the closed operation set can propose on an item.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DerivativeOp {
    /// Fold away completed ephemeral content.
    Fold,
    /// Semantic compaction of a region.
    Compact,
    /// Tail summarization at the last reclamation rung.
    Condense,
    /// Preservation-aware digest with a raw handle.
    Digest,
    /// Replace an item with a store handle.
    DropWithHandle,
    /// Collapse an unpinned range to a provider placeholder.
    PlaceholderCollapse,
}

impl DerivativeOp {
    /// Stable discriminant used in canonical encodings.
    pub fn code(self) -> u64 {
        match self {
            DerivativeOp::Fold => 1,
            DerivativeOp::Compact => 2,
            DerivativeOp::Condense => 3,
            DerivativeOp::Digest => 4,
            DerivativeOp::DropWithHandle => 5,
            DerivativeOp::PlaceholderCollapse => 6,
        }
    }
}

/// Survival-set class the validation gate selects for a lane.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SurvivalClass {
    /// Inside the minimum legal projection, never dropped.
    Protected,
    /// Carried verbatim by the stricter survival set.
    Required,
    /// Ranked, best-effort survival.
    BestEffort,
    /// Outside every survival set.
    Excluded,
}

impl SurvivalClass {
    /// Stable discriminant used in canonical encodings.
    pub fn code(self) -> u64 {
        match self {
            SurvivalClass::Protected => 1,
            SurvivalClass::Required => 2,
            SurvivalClass::BestEffort => 3,
            SurvivalClass::Excluded => 4,
        }
    }
}

/// One registry row: the complete policy for one lane at one registry version.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LanePolicy {
    /// Lane this row governs.
    pub lane: Lane,
    /// Target fidelity for items in this lane.
    pub fidelity: Fidelity,
    /// Derivative operations permitted on items in this lane.
    pub permitted: Vec<DerivativeOp>,
    /// Droppability rank; rank 1 is reclaimed first.
    pub droppability_rank: u64,
    /// Survival-set class used by the validation gate.
    pub survival: SurvivalClass,
    /// Lane floor inside the protected budget, in accounting units.
    pub floor_units: u64,
}

impl LanePolicy {
    /// Encodes the row into `sink`.
    pub fn encode(&self, sink: &mut Sink) {
        sink.tag(self.lane.name());
        sink.int(self.fidelity.code());
        sink.int(self.droppability_rank);
        sink.int(self.survival.code());
        sink.int(self.floor_units);
        for operation in &self.permitted {
            sink.int(operation.code());
        }
    }
}

/// Errors raised by registry lookups.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum PolicyError {
    /// The requested registry version is not resolvable.
    UnsupportedVersion { requested: u64, latest: u64 },
    /// The registry has no row for the lane.
    MissingRow { lane: &'static str },
}

/// Latest registry version resolvable for the session's life.
pub const LANE_POLICY_LATEST_VERSION: u64 = 2;

/// Versioned registry assigning each lane its retention policy.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LanePolicyRegistry {
    version: u64,
    policies: Vec<LanePolicy>,
}

impl LanePolicyRegistry {
    /// Resolves a complete registry for `version`. Every historical version stays
    /// resolvable because durable artifacts are pinned to the version that
    /// produced them.
    pub fn resolve(version: u64) -> Result<Self, PolicyError> {
        match version {
            1 => Ok(Self::build(1, Fidelity::FieldPreserving, 0)),
            2 => Ok(Self::build(2, Fidelity::Digest, 256)),
            other => Err(PolicyError::UnsupportedVersion {
                requested: other,
                latest: LANE_POLICY_LATEST_VERSION,
            }),
        }
    }

    /// Resolves the latest registry.
    pub fn latest() -> Self {
        Self::resolve(LANE_POLICY_LATEST_VERSION)
            .unwrap_or_else(|_| Self::build(LANE_POLICY_LATEST_VERSION, Fidelity::Digest, 256))
    }

    /// Registry version.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Returns the policy row for `lane`.
    pub fn policy(&self, lane: Lane) -> Result<&LanePolicy, PolicyError> {
        self.policies
            .iter()
            .find(|row| row.lane == lane)
            .ok_or(PolicyError::MissingRow { lane: lane.name() })
    }

    /// Encodes the registry into `sink`.
    pub fn encode(&self, sink: &mut Sink) {
        sink.tag("lane-policy-registry");
        sink.int(self.version);
        for row in &self.policies {
            row.encode(sink);
        }
    }

    fn build(version: u64, evidential: Fidelity, evidential_floor: u64) -> Self {
        Self {
            version,
            policies: vec![
                LanePolicy {
                    lane: Lane::Constitutional,
                    fidelity: Fidelity::Verbatim,
                    permitted: Vec::new(),
                    droppability_rank: 4,
                    survival: SurvivalClass::Protected,
                    floor_units: 512,
                },
                LanePolicy {
                    lane: Lane::Decisional,
                    fidelity: Fidelity::FieldPreserving,
                    permitted: vec![DerivativeOp::Fold, DerivativeOp::Compact],
                    droppability_rank: 3,
                    survival: SurvivalClass::Required,
                    floor_units: 128,
                },
                LanePolicy {
                    lane: Lane::Evidential,
                    fidelity: evidential,
                    permitted: vec![
                        DerivativeOp::Fold,
                        DerivativeOp::Compact,
                        DerivativeOp::Condense,
                        DerivativeOp::Digest,
                        DerivativeOp::DropWithHandle,
                        DerivativeOp::PlaceholderCollapse,
                    ],
                    droppability_rank: 2,
                    survival: SurvivalClass::BestEffort,
                    floor_units: evidential_floor,
                },
                LanePolicy {
                    lane: Lane::Ephemeral,
                    fidelity: Fidelity::Droppable,
                    permitted: vec![DerivativeOp::Fold, DerivativeOp::DropWithHandle],
                    droppability_rank: 1,
                    survival: SurvivalClass::Excluded,
                    floor_units: 0,
                },
            ],
        }
    }
}
