//! Deterministic reducer: folds a recorded event log into typed state.
//!
//! The reducer reads recorded attributes only, so replaying an event prefix yields
//! byte-identical typed state and an identical hash. Every input to structure is an
//! event; nothing about a projection can reach back into the reducer.

use crate::context_kernel::canonical::{digest, Digest, Sink};
use crate::context_kernel::events::{
    structural_lane, EventKind, EventLog, OperationClass, RecordedEvent,
};
use crate::context_kernel::ir::{
    normalize, slice_into, ConversationIr, Item, ItemId, Region, StoreRange,
};
use crate::context_kernel::lanes::{LanePolicyRegistry, PolicyError, LANE_POLICY_LATEST_VERSION};
use crate::context_kernel::legality::QuotingConvention;
use crate::context_kernel::scopes::{ScopeError, ScopeId, ScopeRegistry};

/// Log window read by the scope-idleness predicate.
pub const IDLENESS_WINDOW: u64 = 32;
/// State version of a freshly initialized log, and the baseline registry version.
pub const INITIAL_VERSION: u64 = 1;
/// Highest filter-rule registry version the reducer resolves.
pub const FILTER_RULE_LATEST_VERSION: u64 = 1;
/// Highest vocabulary registry version the reducer resolves.
pub const VOCABULARY_LATEST_VERSION: u64 = 1;
/// Highest store-mode code the store defines: 1 normal, 2 read-only, 3 unavailable.
pub const STORE_MODE_UNAVAILABLE: u64 = 3;

/// Errors raised while folding events.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ReducerError {
    /// The conversation IR rejected the operation.
    Ir(crate::context_kernel::ir::IrError),
    /// The scope registry rejected the operation.
    Scope(ScopeError),
    /// The lane-policy registry could not resolve a committed version.
    Policy(PolicyError),
    /// An operation argument named a region rank that does not exist.
    UnknownRegion {
        /// Rank carried by the operation.
        rank: u64,
    },
    /// A compare-and-commit named a parent version other than the current one.
    VersionConflict {
        /// Parent version the operation claimed.
        claimed_parent: u64,
        /// Version the state actually holds.
        actual: u64,
    },
    /// A Phase 2 row whose executor lands in a later phase; the commit is refused.
    OperationNotLanded {
        /// Stable name of the refused row.
        operation: &'static str,
    },
    /// A registry row named a version this build cannot resolve.
    UnsupportedVersion {
        /// Version carried by the operation.
        requested: u64,
        /// Highest version this build resolves.
        latest: u64,
    },
    /// A store-mode row named a mode the store does not define.
    UnknownStoreMode {
        /// Mode code carried by the operation.
        found: u64,
    },
    /// An event was written under a different context-store version.
    StoreVersion {
        /// Version the state holds.
        state: u64,
        /// Version carried by the event.
        event: u64,
    },
}

/// Fully typed state: everything the projection is derived from.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TypedState {
    /// Conversation intermediate representation.
    pub conversation_ir: ConversationIr,
    /// Scope registry with log-derived lineage and activity.
    pub scope_registry: ScopeRegistry,
    /// Lane-policy registry pinned to the committed version.
    pub lane_policy_registry: LanePolicyRegistry,
    /// Compare-and-commit version of this state.
    pub version: u64,
    /// Canonical hash of the state.
    pub state_hash: Digest,
    /// Context-store version the log was written under.
    pub store_version: u64,
    /// Store version selected by a committed migration, if any.
    pub selected_store_version: Option<u64>,
    /// Items protected from reclamation.
    pub pins: Vec<ItemId>,
    /// Quoting convention applied to verbatim lanes.
    pub quoting_convention: QuotingConvention,
    /// Committed filter-rule registry version.
    pub filter_rule_version: u64,
    /// Committed vocabulary registry version.
    pub vocabulary_version: u64,
    /// Store mode code: 1 normal, 2 read-only, 3 unavailable.
    pub store_mode: u64,
    /// Whether the store quiesced against an unwritable mode.
    pub quiesced: bool,
    /// Next free offset on the store spine.
    next_offset: u64,
    /// Identities of events already folded, for deduplication.
    applied: Vec<(u64, Digest)>,
}

impl TypedState {
    /// Builds the state an empty log folds into.
    pub fn genesis(idleness_window: u64, store_version: u64) -> Self {
        let mut state = Self {
            conversation_ir: ConversationIr::new(),
            scope_registry: ScopeRegistry::new(idleness_window),
            lane_policy_registry: LanePolicyRegistry::resolve(INITIAL_VERSION)
                .unwrap_or_else(|_| LanePolicyRegistry::latest()),
            version: INITIAL_VERSION,
            state_hash: 0,
            store_version,
            selected_store_version: None,
            pins: Vec::new(),
            quoting_convention: QuotingConvention::Fenced,
            filter_rule_version: FILTER_RULE_LATEST_VERSION,
            vocabulary_version: VOCABULARY_LATEST_VERSION,
            store_mode: 1,
            quiesced: false,
            next_offset: 0,
            applied: Vec::new(),
        };
        state.state_hash = Reducer::new(idleness_window).hash(&state);
        state
    }

    /// Whether an event identity has already been folded.
    pub fn is_applied(&self, sequence: u64, body_digest: Digest) -> bool {
        self.applied
            .iter()
            .any(|identity| identity.0 == sequence && identity.1 == body_digest)
    }

    /// Number of events folded into this state.
    pub fn applied_len(&self) -> usize {
        self.applied.len()
    }

    /// Encodes the state into `sink`.
    pub fn encode(&self, sink: &mut Sink) {
        sink.tag("typed-state");
        sink.int(self.version);
        sink.int(self.store_version);
        sink.int(self.next_offset);
        self.conversation_ir.encode(sink);
        self.scope_registry.encode(sink);
        self.lane_policy_registry.encode(sink);
        sink.tag(self.quoting_convention.name());
        sink.int(self.filter_rule_version);
        sink.int(self.vocabulary_version);
        sink.int(self.store_mode);
        sink.tag(if self.quiesced { "quiesced" } else { "live" });
        match self.selected_store_version {
            Some(version) => sink.int(version),
            None => sink.int(0),
        }
        for pin in &self.pins {
            pin.encode(sink);
        }
    }
}

/// Folds recorded events into typed state.
pub struct Reducer {
    idleness_window: u64,
}

impl Reducer {
    /// Creates a reducer whose scope-idleness predicate reads `idleness_window` events.
    pub fn new(idleness_window: u64) -> Self {
        Self { idleness_window }
    }

    /// Folds an entire log into a fresh state.
    pub fn fold(&self, log: &EventLog) -> Result<TypedState, ReducerError> {
        let mut state = TypedState::genesis(self.idleness_window, log.store_version());
        self.fold_from(&mut state, log)?;
        state.state_hash = self.hash(&state);
        Ok(state)
    }

    /// Folds `log` into an existing state, skipping event identities already applied.
    pub fn fold_from(&self, state: &mut TypedState, log: &EventLog) -> Result<(), ReducerError> {
        for event in log.events() {
            if state.is_applied(event.sequence, event.body_digest) {
                continue;
            }
            if event.store_version != state.store_version {
                return Err(ReducerError::StoreVersion {
                    state: state.store_version,
                    event: event.store_version,
                });
            }
            self.apply(state, event)?;
            state.applied.push((event.sequence, event.body_digest));
        }
        state.state_hash = self.hash(state);
        Ok(())
    }

    /// Canonical hash of a state.
    pub fn hash(&self, state: &TypedState) -> Digest {
        let mut sink = Sink::new();
        state.encode(&mut sink);
        digest(&sink.finish())
    }

    fn apply(&self, state: &mut TypedState, event: &RecordedEvent) -> Result<(), ReducerError> {
        match &event.kind {
            EventKind::Append {
                source,
                sanitized,
                scope,
            } => apply_append(state, event, source, sanitized, *scope),
            EventKind::Ledger { .. } => Ok(()),
            EventKind::OperationCommit {
                class,
                subject,
                argument,
            } => apply_operation(state, event, class, *subject, *argument),
            EventKind::ProviderTurn { .. } => Ok(()),
        }
    }
}

fn apply_append(
    state: &mut TypedState,
    event: &RecordedEvent,
    source: &crate::context_kernel::events::AppendSource,
    sanitized: &[u8],
    scope: ScopeId,
) -> Result<(), ReducerError> {
    let length = sanitized.len() as u64;
    let provenance = vec![StoreRange {
        offset: state.next_offset,
        length,
    }];
    state.next_offset += length;
    attribute(state, scope, event.sequence)?;
    let id = ItemId::new(event.sequence);
    let lane = structural_lane(source);
    let item = Item::new(id, lane, provenance, scope);
    state
        .conversation_ir
        .insert(item)
        .map_err(ReducerError::Ir)?;
    Ok(())
}

fn apply_operation(
    state: &mut TypedState,
    event: &RecordedEvent,
    class: &OperationClass,
    subject: u64,
    argument: u64,
) -> Result<(), ReducerError> {
    match class {
        OperationClass::ScopeOpen => {
            let parent = if argument == 0 { None } else { Some(argument) };
            state
                .scope_registry
                .open(subject, parent, event.sequence)
                .map_err(ReducerError::Scope)?;
        }
        OperationClass::ScopeCloseByEvent => {
            state
                .scope_registry
                .close_by_event(subject, event.sequence)
                .map_err(ReducerError::Scope)?;
        }
        OperationClass::ScopeCloseByDeclaration => {
            state
                .scope_registry
                .close_by_declaration(subject, event.sequence)
                .map_err(ReducerError::Scope)?;
        }
        OperationClass::Resegment => resegment(state, subject, argument)?,
        OperationClass::Place => place(state, subject, argument)?,
        OperationClass::Unplace => {
            state
                .conversation_ir
                .unplace(ItemId::new(subject))
                .map_err(ReducerError::Ir)?;
        }
        OperationClass::Pin => {
            let id = ItemId::new(subject);
            if !state.pins.contains(&id) {
                state.pins.push(id);
            }
        }
        OperationClass::Unpin => {
            state.pins.retain(|pin| pin.value() != subject);
        }
        OperationClass::LanePolicyUpdate => update_policy(state, subject, argument)?,
        OperationClass::MigrationSelect => {
            state.selected_store_version = Some(subject);
            state.store_version = subject;
        }
        OperationClass::AdmitIngress => admit_ingress(state, event, subject, argument)?,
        OperationClass::Sanitize | OperationClass::Import | OperationClass::IndexRebuild => {
            return Err(not_landed(class));
        }
        OperationClass::Redact => redact_item(state, subject)?,
        OperationClass::RuleUpdate => commit_registry(
            state,
            subject,
            argument,
            FILTER_RULE_LATEST_VERSION,
            |state: &mut TypedState| &mut state.filter_rule_version,
        )?,
        OperationClass::VocabularyUpdate => commit_registry(
            state,
            subject,
            argument,
            VOCABULARY_LATEST_VERSION,
            |state: &mut TypedState| &mut state.vocabulary_version,
        )?,
        OperationClass::StoreMode => commit_store_mode(state, subject)?,
        OperationClass::QuiesceUnwritable => state.quiesced = true,
    }
    Ok(())
}

/// Admits an ingress transaction: the argument is the sanitized byte length, the
/// subject is the scope the payload was ingested into.
fn admit_ingress(
    state: &mut TypedState,
    event: &RecordedEvent,
    subject: u64,
    argument: u64,
) -> Result<(), ReducerError> {
    state.next_offset += argument;
    attribute(state, subject, event.sequence)
}

/// Moves an item to the vault side: unplaced and unpinned, byte provenance kept.
fn redact_item(state: &mut TypedState, subject: u64) -> Result<(), ReducerError> {
    state.pins.retain(|pin| pin.value() != subject);
    state
        .conversation_ir
        .unplace(ItemId::new(subject))
        .map_err(ReducerError::Ir)
}

/// Typed refusal for a row whose executor lands in a later phase.
fn not_landed(class: &OperationClass) -> ReducerError {
    ReducerError::OperationNotLanded {
        operation: class.name(),
    }
}

/// Commits a registry-version row by compare-and-commit on the parent version.
fn commit_registry(
    state: &mut TypedState,
    subject: u64,
    argument: u64,
    latest: u64,
    committed: fn(&mut TypedState) -> &mut u64,
) -> Result<(), ReducerError> {
    if argument != state.version {
        return Err(ReducerError::VersionConflict {
            claimed_parent: argument,
            actual: state.version,
        });
    }
    if subject == 0 || subject > latest {
        return Err(ReducerError::UnsupportedVersion {
            requested: subject,
            latest,
        });
    }
    *committed(state) = subject;
    state.version = subject;
    Ok(())
}

/// Commits a store-mode row; the mode code must be one the store defines.
fn commit_store_mode(state: &mut TypedState, subject: u64) -> Result<(), ReducerError> {
    if !(1..=STORE_MODE_UNAVAILABLE).contains(&subject) {
        return Err(ReducerError::UnknownStoreMode { found: subject });
    }
    state.store_mode = subject;
    Ok(())
}

fn resegment(state: &mut TypedState, subject: u64, argument: u64) -> Result<(), ReducerError> {
    let id = ItemId::new(subject);
    let parent = state
        .conversation_ir
        .item(id)
        .map_err(ReducerError::Ir)?
        .clone();
    let parts = argument.max(1) as usize;
    let ranges = normalize(&parent.provenance);
    state
        .conversation_ir
        .split(id, slice_into(&ranges, parts))
        .map_err(ReducerError::Ir)?;
    state.next_offset = state.next_offset.max(parent.byte_range.end());
    Ok(())
}

fn place(state: &mut TypedState, subject: u64, argument: u64) -> Result<(), ReducerError> {
    let region =
        Region::from_rank(argument).ok_or(ReducerError::UnknownRegion { rank: argument })?;
    state
        .conversation_ir
        .place(ItemId::new(subject), region)
        .map_err(ReducerError::Ir)?;
    Ok(())
}

fn update_policy(state: &mut TypedState, subject: u64, argument: u64) -> Result<(), ReducerError> {
    if argument != state.version {
        return Err(ReducerError::VersionConflict {
            claimed_parent: argument,
            actual: state.version,
        });
    }
    if subject > LANE_POLICY_LATEST_VERSION {
        return Err(ReducerError::Policy(PolicyError::UnsupportedVersion {
            requested: subject,
            latest: LANE_POLICY_LATEST_VERSION,
        }));
    }
    state.lane_policy_registry =
        LanePolicyRegistry::resolve(subject).unwrap_or_else(|_| LanePolicyRegistry::latest());
    state.version = subject;
    Ok(())
}

fn attribute(state: &mut TypedState, scope: ScopeId, sequence: u64) -> Result<(), ReducerError> {
    match state.scope_registry.attribute_item(scope, sequence) {
        Ok(()) => Ok(()),
        Err(ScopeError::UnknownScope { id }) => state
            .scope_registry
            .open(id, None, sequence)
            .map_err(ReducerError::Scope),
        Err(error) => Err(ReducerError::Scope(error)),
    }
}
