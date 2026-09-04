//! Append-only event log: total order, sequence numbers, checksums, identities.

use crate::context_kernel::canonical::{Digest, HashScope, Sink};
use crate::context_kernel::ir::SegmentClaim;
use crate::context_kernel::lanes::Lane;
use crate::context_kernel::scopes::ScopeId;

/// Schema version of the event record itself.
pub const EVENT_SCHEMA_VERSION: u64 = 1;
/// Sequence number of the first event in a log.
pub const FIRST_SEQUENCE: u64 = 1;
/// Chain value of an empty log.
pub const GENESIS_CHECKSUM: Digest = 0;

/// Source of a sanitized append admitted by the ingress transaction.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum AppendSource {
    /// Operator or user message.
    User,
    /// Assistant-authored message.
    Assistant,
    /// Tool result paired with a call identity.
    ToolResult {
        /// Call identity the result answers.
        call_id: String,
        /// Tool identity declared by the harness.
        tool: String,
    },
}

impl AppendSource {
    /// Encodes the source into `sink`.
    pub fn encode(&self, sink: &mut Sink) {
        match self {
            AppendSource::User => sink.tag("user"),
            AppendSource::Assistant => sink.tag("assistant"),
            AppendSource::ToolResult { call_id, tool } => {
                sink.tag("tool-result");
                sink.blob(call_id.as_bytes());
                sink.blob(tool.as_bytes());
            }
        }
    }
}

/// Ledger events sequenced in the same total order as appends.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LedgerEventKind {
    /// An obligation was admitted under the authority grammar.
    ObligationAdmitted,
    /// An obligation was discharged by a gate-supported resolution.
    ObligationDischarged,
    /// A convenience memory was admitted above the utility threshold.
    ConvenienceAdmitted,
    /// A convenience memory decayed below the threshold and retired.
    ConvenienceRetired,
}

impl LedgerEventKind {
    /// Stable name used in canonical encodings.
    pub fn name(&self) -> &'static str {
        match self {
            LedgerEventKind::ObligationAdmitted => "obligation-admitted",
            LedgerEventKind::ObligationDischarged => "obligation-discharged",
            LedgerEventKind::ConvenienceAdmitted => "convenience-admitted",
            LedgerEventKind::ConvenienceRetired => "convenience-retired",
        }
    }
}

/// Classes of operation commits the reducer understands.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum OperationClass {
    /// Harness scope event admitting scope-open.
    ScopeOpen,
    /// Harness scope event admitting scope-close.
    ScopeCloseByEvent,
    /// Declare-boundary closure.
    ScopeCloseByDeclaration,
    /// Logged resegment producing claim-atomic children.
    Resegment,
    /// Region placement of an item.
    Place,
    /// Store-only transition of an item.
    Unplace,
    /// Pin registration.
    Pin,
    /// Pin release.
    Unpin,
    /// Lane-policy registry version change.
    LanePolicyUpdate,
    /// Context-store migration selection.
    MigrationSelect,
    /// Ingress transaction admitted a sanitized payload into a scope.
    AdmitIngress,
    /// In-place spine rewrite of a captured item.
    Sanitize,
    /// Item plaintext moved to the vault; only a reference remains.
    Redact,
    /// External bytes imported into the store spine.
    Import,
    /// Filter-rule registry version change.
    RuleUpdate,
    /// Vocabulary registry version change.
    VocabularyUpdate,
    /// Retrieval index rebuilt from the spine.
    IndexRebuild,
    /// Store mode change.
    StoreMode,
    /// Store quiesced against an unwritable mode.
    QuiesceUnwritable,
}

impl OperationClass {
    /// Stable name used in canonical encodings.
    pub fn name(&self) -> &'static str {
        match self {
            OperationClass::ScopeOpen => "scope-open",
            OperationClass::ScopeCloseByEvent => "scope-close-by-event",
            OperationClass::ScopeCloseByDeclaration => "scope-close-by-declaration",
            OperationClass::Resegment => "resegment",
            OperationClass::Place => "place",
            OperationClass::Unplace => "unplace",
            OperationClass::Pin => "pin",
            OperationClass::Unpin => "unpin",
            OperationClass::LanePolicyUpdate => "lane-policy-update",
            OperationClass::MigrationSelect => "migration-select",
            OperationClass::AdmitIngress => "admit-ingress",
            OperationClass::Sanitize => "sanitize",
            OperationClass::Redact => "redact",
            OperationClass::Import => "import",
            OperationClass::RuleUpdate => "rule-update",
            OperationClass::VocabularyUpdate => "vocabulary-update",
            OperationClass::IndexRebuild => "index-rebuild",
            OperationClass::StoreMode => "store-mode",
            OperationClass::QuiesceUnwritable => "quiesce-unwritable",
        }
    }
}

/// Provider turn classes that carry the current epoch.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ProviderTurnKind {
    /// Conversation turn.
    Conversation,
    /// Management-plane call.
    Management,
}

/// One event in the total order.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum EventKind {
    /// Sanitized append from the ingress transaction.
    Append {
        /// Structural source of the bytes.
        source: AppendSource,
        /// Sanitized bytes appended to the store spine.
        sanitized: Vec<u8>,
        /// Scope active at ingestion.
        scope: ScopeId,
        /// Claim-atomic segmentation of `sanitized`, as recorded at ingestion.
        /// An empty list is the pre-segmentation append: the whole payload is one
        /// claim with no recorded class, so its lane falls back to the source.
        claims: Vec<SegmentClaim>,
    },
    /// Fact-ledger event.
    Ledger {
        /// Ledger event class.
        kind: LedgerEventKind,
        /// Ledger entry identity.
        key: String,
    },
    /// Operation commit written by the transaction core.
    OperationCommit {
        /// Operation class.
        class: OperationClass,
        /// Subject identity: scope id, item id, or registry version.
        subject: u64,
        /// Argument: parent scope, region rank, part count, or selected version.
        argument: u64,
    },
    /// Provider turn admitted by the egress gateway.
    ProviderTurn {
        /// Turn class.
        kind: ProviderTurnKind,
        /// Conservative bound of the materialized request.
        request_units: u64,
    },
}

impl EventKind {
    /// Encodes the event body into `sink`.
    pub fn encode(&self, sink: &mut Sink) {
        match self {
            EventKind::Append {
                source,
                sanitized,
                scope,
                claims,
            } => {
                sink.tag("append");
                source.encode(sink);
                sink.blob(sanitized);
                sink.int(*scope);
                sink.int(claims.len() as u64);
                for claim in claims {
                    sink.int(claim.span.offset);
                    sink.int(claim.span.length);
                    match claim.class {
                        Some(class) => class.encode(sink),
                        None => sink.tag("unclassified"),
                    }
                }
            }
            EventKind::Ledger { kind, key } => {
                sink.tag("ledger");
                sink.tag(kind.name());
                sink.blob(key.as_bytes());
            }
            EventKind::OperationCommit {
                class,
                subject,
                argument,
            } => {
                sink.tag("operation");
                sink.tag(class.name());
                sink.int(*subject);
                sink.int(*argument);
            }
            EventKind::ProviderTurn {
                kind,
                request_units,
            } => {
                sink.tag("provider-turn");
                sink.int(match kind {
                    ProviderTurnKind::Conversation => 1,
                    ProviderTurnKind::Management => 2,
                });
                sink.int(*request_units);
            }
        }
    }
}

/// Structural lane fallback for an append whose claims carry no recorded class.
/// Lane assignment is decided by segmentation wherever it classified any claim;
/// the source is the documented fallback, never the primary rule: a document
/// pasted into a user message is classified by its own content, not by the
/// message that carried it.
pub fn structural_lane(source: &AppendSource) -> Lane {
    match source {
        AppendSource::User => Lane::Constitutional,
        AppendSource::Assistant => Lane::Decisional,
        AppendSource::ToolResult { .. } => Lane::Evidential,
    }
}

/// Identity of one recorded event: its sequence plus the digest of its canonical
/// body. Re-proposals and replays are deduplicated on this pair.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EventIdentity {
    /// Log sequence of the event.
    pub sequence: u64,
    /// Digest of the canonical event body.
    pub body_digest: Digest,
}

/// One recorded event with its checksum and recorded wall-clock attribute.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RecordedEvent {
    /// Log sequence; with the epoch it names a unique position in the total order.
    pub sequence: u64,
    /// Epoch carried by the writer.
    pub epoch: u64,
    /// Wall-clock timestamp recorded as an event attribute.
    pub recorded_unix_ms: u64,
    /// Schema version of the record.
    pub schema_version: u64,
    /// Context-store version the writer was on.
    pub store_version: u64,
    /// Event body.
    pub kind: EventKind,
    /// Digest of the canonical body, excluding the chain value.
    pub body_digest: Digest,
    /// Chain checksum committing to every predecessor.
    pub checksum: Digest,
}

impl RecordedEvent {
    /// Identity of the event.
    pub fn identity(&self) -> EventIdentity {
        EventIdentity {
            sequence: self.sequence,
            body_digest: self.body_digest,
        }
    }

    /// Canonical body bytes: everything except the chain checksum.
    pub fn encode_body(&self) -> Vec<u8> {
        let mut sink = Sink::new();
        sink.int(self.sequence);
        sink.byte_order_mark();
        sink.int(self.epoch);
        sink.int(self.recorded_unix_ms);
        sink.int(self.schema_version);
        sink.int(self.store_version);
        self.kind.encode(&mut sink);
        sink.finish()
    }

    /// Builds a fully checked event for `previous` chain value.
    pub fn seal(
        sequence: u64,
        epoch: u64,
        recorded_unix_ms: u64,
        store_version: u64,
        kind: EventKind,
        previous: Digest,
    ) -> Self {
        let mut event = Self {
            sequence,
            epoch,
            recorded_unix_ms,
            schema_version: EVENT_SCHEMA_VERSION,
            store_version,
            kind,
            body_digest: 0,
            checksum: 0,
        };
        event.body_digest = HashScope::EventChain.digest(&event.encode_body());
        event.checksum = HashScope::EventChain.chain(previous, &event.encode_body());
        event
    }

    /// Whether the event's chain checksum matches `previous`.
    pub fn verify(&self, previous: Digest) -> bool {
        self.body_digest == HashScope::EventChain.digest(&self.encode_body())
            && self.checksum == HashScope::EventChain.chain(previous, &self.encode_body())
    }
}

/// Single writer assigning the total order over appends, ledger events, operation
/// commits, and provider turns.
///
/// The sequencer never reads a live clock. Callers advance the recorded timestamp
/// through [`Sequencer::advance_clock`] or supply it per event with
/// [`Sequencer::append_at`]; replay reads recorded values only.
pub struct Sequencer {
    next_sequence: u64,
    last_checksum: Digest,
    recorded_unix_ms: u64,
    epoch: u64,
}

impl Sequencer {
    /// Creates a sequencer at the genesis of an empty log.
    pub fn new(next_sequence: u64, epoch: u64, recorded_unix_ms: u64) -> Self {
        Self {
            next_sequence,
            last_checksum: GENESIS_CHECKSUM,
            recorded_unix_ms,
            epoch,
        }
    }

    /// Creates a sequencer resuming a durable prefix: the chain continues from
    /// `last_checksum` — the head of the recorded prefix, or [`GENESIS_CHECKSUM`]
    /// when resuming an empty log — instead of restarting at genesis. A sequencer
    /// that restarts the chain at genesis over an existing prefix emits events no
    /// replay can verify.
    pub fn resume(
        next_sequence: u64,
        last_checksum: Digest,
        epoch: u64,
        recorded_unix_ms: u64,
    ) -> Self {
        Self {
            next_sequence,
            last_checksum,
            recorded_unix_ms,
            epoch,
        }
    }

    /// Creates a sequencer that continues the chain of `log`: the first event it
    /// appends follows `log`'s head checksum and carries the next sequence number.
    pub fn continuing(log: &EventLog, epoch: u64, recorded_unix_ms: u64) -> Self {
        Self::resume(
            log.len() as u64 + FIRST_SEQUENCE,
            log.head_checksum(),
            epoch,
            recorded_unix_ms,
        )
    }

    /// Recorded chain value the next appended event commits to.
    pub fn last_checksum(&self) -> Digest {
        self.last_checksum
    }

    /// Records the caller-supplied timestamp used for subsequent appends.
    pub fn advance_clock(&mut self, recorded_unix_ms: u64) {
        self.recorded_unix_ms = recorded_unix_ms;
    }

    /// Next sequence number that will be assigned.
    pub fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    /// Appends an event using the last recorded timestamp.
    pub fn append(&mut self, kind: EventKind, store_version: u64) -> RecordedEvent {
        let recorded_unix_ms = self.recorded_unix_ms;
        self.append_at(kind, store_version, recorded_unix_ms)
    }

    /// Appends an event with an explicit recorded timestamp.
    pub fn append_at(
        &mut self,
        kind: EventKind,
        store_version: u64,
        recorded_unix_ms: u64,
    ) -> RecordedEvent {
        let event = RecordedEvent::seal(
            self.next_sequence,
            self.epoch,
            recorded_unix_ms,
            store_version,
            kind,
            self.last_checksum,
        );
        self.next_sequence += 1;
        self.last_checksum = event.checksum;
        self.recorded_unix_ms = recorded_unix_ms;
        event
    }
}

/// Errors raised by log appends.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LogError {
    /// The appended event does not continue the total order.
    SequenceGap { expected: u64, actual: u64 },
    /// The sequence is already present in the log.
    DuplicateSequence { sequence: u64 },
    /// The chain checksum does not commit to the recorded prefix.
    ChecksumMismatch { sequence: u64 },
    /// The record's schema version is not the log's schema version.
    SchemaVersion { sequence: u64, found: u64 },
    /// The record was written under a different context-store version.
    StoreVersion { sequence: u64, log: u64, event: u64 },
}

/// Append-only log of recorded events.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct EventLog {
    events: Vec<RecordedEvent>,
    store_version: u64,
}

impl EventLog {
    /// Creates an empty log for `store_version`.
    pub fn new(store_version: u64) -> Self {
        Self {
            events: Vec::new(),
            store_version,
        }
    }

    /// Context-store version this log was written under.
    pub fn store_version(&self) -> u64 {
        self.store_version
    }

    /// Number of recorded events.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether the log is empty.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// All recorded events in total order.
    pub fn events(&self) -> &[RecordedEvent] {
        &self.events
    }

    /// Chain value of the recorded prefix.
    pub fn head_checksum(&self) -> Digest {
        match self.events.last() {
            Some(event) => event.checksum,
            None => GENESIS_CHECKSUM,
        }
    }

    /// Appends an event after validating continuity, checksum chain, and versions.
    pub fn append(&mut self, event: RecordedEvent) -> Result<u64, LogError> {
        let expected = self.len() as u64 + FIRST_SEQUENCE;
        if event.schema_version != EVENT_SCHEMA_VERSION {
            return Err(LogError::SchemaVersion {
                sequence: event.sequence,
                found: event.schema_version,
            });
        }
        if event.store_version != self.store_version {
            return Err(LogError::StoreVersion {
                sequence: event.sequence,
                log: self.store_version,
                event: event.store_version,
            });
        }
        if event.sequence != expected {
            return Err(LogError::SequenceGap {
                expected,
                actual: event.sequence,
            });
        }
        if !event.verify(self.head_checksum()) {
            return Err(LogError::ChecksumMismatch {
                sequence: event.sequence,
            });
        }
        self.events.push(event.clone());
        Ok(event.sequence)
    }

    /// The first `count` events as an independent log.
    pub fn prefix(&self, count: usize) -> EventLog {
        EventLog {
            events: self.events.iter().take(count).cloned().collect(),
            store_version: self.store_version,
        }
    }

    /// Replay clock holding the recorded timestamps of the log.
    pub fn replay_clock(&self) -> ReplayClock {
        ReplayClock {
            stamps: self
                .events
                .iter()
                .map(|event| (event.sequence, event.recorded_unix_ms))
                .collect(),
        }
    }
}

/// Recorded timestamps only. Replay never reads a live clock.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ReplayClock {
    stamps: Vec<(u64, u64)>,
}

impl ReplayClock {
    /// Timestamp recorded for `sequence`.
    pub fn recorded_unix_ms(&self, sequence: u64) -> Option<u64> {
        self.stamps
            .iter()
            .find(|stamp| stamp.0 == sequence)
            .map(|stamp| stamp.1)
    }

    /// Most recently recorded timestamp.
    pub fn last_recorded_unix_ms(&self) -> u64 {
        match self.stamps.last() {
            Some(stamp) => stamp.1,
            None => 0,
        }
    }
}
