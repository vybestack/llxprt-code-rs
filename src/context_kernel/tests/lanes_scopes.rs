//! Scope, lane-policy, hash-scope, and lane-derivation tests.

use crate::context_kernel::events::{
    AppendSource, EventKind, EventLog, OperationClass, RecordedEvent, Sequencer, FIRST_SEQUENCE,
};
use crate::context_kernel::ir::{ItemId, ItemNamespace, SegmentClaim, StoreRange, StructuralClass};
use crate::context_kernel::lanes::{
    Lane, LanePolicyRegistry, PolicyError, LANE_POLICY_LATEST_VERSION,
};
use crate::context_kernel::migration::V2;
use crate::context_kernel::reducer::{Reducer, ReducerError, IDLENESS_WINDOW};
use crate::context_kernel::scopes::{ScopeError, ScopeState};

fn append(kind: EventKind, sequencer: &mut Sequencer, log: &mut EventLog) -> RecordedEvent {
    let event = sequencer.append(kind, log.store_version());
    let expected = log.len() as u64 + FIRST_SEQUENCE;
    assert_eq!(
        event.sequence, expected,
        "fixture continues the total order"
    );
    log.append(event.clone()).unwrap();
    event
}

fn user(text: &str, scope: u64) -> EventKind {
    EventKind::Append {
        source: AppendSource::User,
        sanitized: text.as_bytes().to_vec(),
        scope,
        claims: Vec::new(),
    }
}
fn op(class: OperationClass, subject: u64, argument: u64) -> EventKind {
    EventKind::OperationCommit {
        class,
        subject,
        argument,
    }
}

/// Spans a segmented claim list must carry: consecutive offsets over `payload`,
/// so the claims cover the append exactly.
fn contiguous(payload: &[u8], part_lengths: &[u64]) -> Vec<SegmentClaim> {
    let mut offset = 0_u64;
    let mut claims = Vec::with_capacity(part_lengths.len());
    for length in part_lengths {
        claims.push(SegmentClaim {
            span: StoreRange {
                offset,
                length: *length,
            },
            class: None,
        });
        offset += length;
    }
    assert_eq!(
        offset as usize,
        payload.len(),
        "the claims must cover the payload exactly"
    );
    claims
}

#[test]
fn scopes_track_nesting_lifecycle_and_idleness_from_the_log() {
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut log = EventLog::new(V2);
    append(
        op(OperationClass::ScopeOpen, 1, 0),
        &mut sequencer,
        &mut log,
    );
    append(
        op(OperationClass::ScopeOpen, 2, 1),
        &mut sequencer,
        &mut log,
    );
    append(user("a", 2), &mut sequencer, &mut log);
    append(
        op(OperationClass::ScopeCloseByEvent, 2, 0),
        &mut sequencer,
        &mut log,
    );
    let state = Reducer::new(4).fold(&log).unwrap();
    assert_eq!(state.scope_registry.children(1), vec![2]);
    assert_eq!(
        state.scope_registry.state(2).unwrap(),
        ScopeState::ClosedByEvent
    );
    assert!(state.scope_registry.state(1).unwrap().is_open());
    assert!(!state.scope_registry.is_idle(2, 9).unwrap());
    assert!(state.scope_registry.is_idle(1, 40).unwrap());
    assert_eq!(
        state.scope_registry.scope(9).err(),
        Some(ScopeError::UnknownScope { id: 9 })
    );
    let mut declared = log;
    append(
        op(OperationClass::ScopeCloseByDeclaration, 1, 0),
        &mut sequencer,
        &mut declared,
    );
    let closed = Reducer::new(4).fold(&declared).unwrap();
    assert_eq!(
        closed.scope_registry.state(1).unwrap(),
        ScopeState::ClosedByDeclaration
    );
    let mut twice = declared;
    append(
        op(OperationClass::ScopeCloseByEvent, 1, 0),
        &mut sequencer,
        &mut twice,
    );
    assert_eq!(
        Reducer::new(4).fold(&twice).unwrap_err(),
        ReducerError::Scope(ScopeError::AlreadyClosed { id: 1 })
    );
}

#[test]
fn lane_policy_registry_resolves_committed_versions() {
    let v1 = LanePolicyRegistry::resolve(1).unwrap();
    let v2 = LanePolicyRegistry::resolve(2).unwrap();
    assert_eq!(v1.version(), 1);
    assert_eq!(v2.version(), 2);
    assert_eq!(
        LanePolicyRegistry::resolve(LANE_POLICY_LATEST_VERSION + 1).unwrap_err(),
        PolicyError::UnsupportedVersion {
            requested: LANE_POLICY_LATEST_VERSION + 1,
            latest: LANE_POLICY_LATEST_VERSION,
        }
    );
    assert_eq!(v1.policy(Lane::Evidential).unwrap().floor_units, 0);
    assert_eq!(v2.policy(Lane::Evidential).unwrap().floor_units, 256);
    assert_eq!(
        v2.policy(Lane::Constitutional).unwrap().survival.code(),
        1,
        "constitutional items stay protected"
    );
}

#[test]
fn hash_scopes_keep_identities_independent() {
    use crate::context_kernel::canonical::HashScope;
    let bytes = b"identical bytes";
    let scopes = [
        HashScope::EventChain,
        HashScope::State,
        HashScope::StoreBuild,
    ];
    let digests: Vec<crate::context_kernel::canonical::Digest> =
        scopes.iter().map(|scope| scope.digest(bytes)).collect();
    assert_ne!(digests[0], digests[1]);
    assert_ne!(digests[1], digests[2]);
    assert_ne!(digests[0], digests[2]);

    let chained = HashScope::EventChain.chain(digests[0], b"next");
    assert_ne!(chained, digests[0], "chaining commits to the predecessor");
    assert_ne!(
        HashScope::State.chain(digests[0], b"next"),
        chained,
        "the same chain in another scope is a different value"
    );
    assert_eq!(
        HashScope::EventChain.digest(bytes),
        digests[0],
        "a scope digest is deterministic"
    );
}

/// GREEN: a document pasted into a user message is classified by its own claims,
/// so it never lands in the Constitutional lane wholesale.
#[test]
fn lanes_come_from_recorded_claims_not_the_message_source() {
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut log = EventLog::new(V2);
    append(
        op(OperationClass::ScopeOpen, 1, 0),
        &mut sequencer,
        &mut log,
    );
    let document = b"exact error span\nnoise: filler line\nfn main() -> {}\n";
    append(
        EventKind::Append {
            source: AppendSource::User,
            sanitized: document.to_vec(),
            scope: 1,
            claims: vec![
                SegmentClaim {
                    span: StoreRange {
                        offset: 0,
                        length: 17,
                    },
                    class: Some(StructuralClass::ExactSpan),
                },
                SegmentClaim {
                    span: StoreRange {
                        offset: 17,
                        length: 19,
                    },
                    class: Some(StructuralClass::Noise),
                },
                SegmentClaim {
                    span: StoreRange {
                        offset: 36,
                        length: 16,
                    },
                    class: Some(StructuralClass::Code),
                },
            ],
        },
        &mut sequencer,
        &mut log,
    );
    let state = Reducer::new(IDLENESS_WINDOW).fold(&log).unwrap();
    let lanes: Vec<Lane> = state
        .conversation_ir
        .items()
        .iter()
        .map(|item| item.lane)
        .collect();
    assert_eq!(
        lanes,
        vec![Lane::Constitutional, Lane::Ephemeral, Lane::Evidential],
        "each claim's lane is decided by its own class, not by the user source"
    );
    let ids: Vec<ItemId> = state
        .conversation_ir
        .items()
        .iter()
        .map(|item| item.id())
        .collect();
    assert_eq!(ids.len(), 3, "one item per recorded claim");
    let units: u64 = state
        .conversation_ir
        .items()
        .iter()
        .map(|item| item.units())
        .sum();
    assert_eq!(
        units as usize,
        document.len(),
        "the claims cover the append"
    );
}

/// GREEN: an append with no recorded claims is the pre-segmentation append, so
/// its lane falls back to the structural source, and the whole payload is one
/// item.
#[test]
fn unsegmented_append_falls_back_to_the_source_lane() {
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut log = EventLog::new(V2);
    append(
        op(OperationClass::ScopeOpen, 1, 0),
        &mut sequencer,
        &mut log,
    );
    append(user("a plain task statement", 1), &mut sequencer, &mut log);
    let state = Reducer::new(IDLENESS_WINDOW).fold(&log).unwrap();
    assert_eq!(state.conversation_ir.len(), 1, "one claim over the payload");
    assert_eq!(
        state.conversation_ir.items()[0].lane,
        Lane::Constitutional,
        "the documented fallback is the source lane"
    );
    assert_eq!(
        state.conversation_ir.items()[0].id(),
        ItemId::append(0),
        "the first append of the log mints the first append identifier, not the event sequence"
    );
}

/// GREEN: a segmented append mints one identifier per claim from the IR's own
/// append mint, so the next event's append never re-mints one. Minting from the
/// event sequence would collide, because sequences are strictly consecutive while
/// one append mints as many identifiers as it has claims.
#[test]
fn segmented_appends_followed_by_other_appends_never_collide() {
    let payload = b"0123456789abcdefghijklmn"; // 24 bytes
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut log = EventLog::new(V2);
    append(
        op(OperationClass::ScopeOpen, 1, 0),
        &mut sequencer,
        &mut log,
    );
    // A three-claim append: three identifiers minted for one event.
    append(
        EventKind::Append {
            source: AppendSource::User,
            sanitized: payload.to_vec(),
            scope: 1,
            claims: contiguous(payload, &[8, 8, 8]),
        },
        &mut sequencer,
        &mut log,
    );
    // Ordinary, unsegmented appends that follow: each mints one identifier whose
    // value the segmented append already used had the mint borrowed the sequence.
    append(user("second append", 1), &mut sequencer, &mut log);
    append(user("third append", 1), &mut sequencer, &mut log);
    append(
        EventKind::Append {
            source: AppendSource::Assistant,
            sanitized: b"a fourth payload".to_vec(),
            scope: 1,
            claims: contiguous(b"a fourth payload", &[9, 7]),
        },
        &mut sequencer,
        &mut log,
    );

    let state = Reducer::new(IDLENESS_WINDOW).fold(&log).unwrap();
    assert_eq!(
        state.conversation_ir.len(),
        7,
        "three claims, then one, one, and two"
    );
    let ids: Vec<ItemId> = state
        .conversation_ir
        .items()
        .iter()
        .map(|item| item.id())
        .collect();
    let unique: std::collections::BTreeSet<ItemId> = ids.iter().copied().collect();
    assert_eq!(
        unique.len(),
        ids.len(),
        "every minted identifier is distinct, including across consecutive events"
    );
    assert_eq!(
        ids,
        vec![
            ItemId::append(0),
            ItemId::append(1),
            ItemId::append(2),
            ItemId::append(3),
            ItemId::append(4),
            ItemId::append(5),
            ItemId::append(6),
        ],
        "the mint raises the watermark once per identifier, never reusing one"
    );
    assert_eq!(
        state
            .conversation_ir
            .namespace_watermark(ItemNamespace::Append),
        7,
        "the watermark counts every minted identifier"
    );
    // Provenance still covers the spine without overlap.
    let units: u64 = state
        .conversation_ir
        .items()
        .iter()
        .map(|item| item.units())
        .sum();
    assert_eq!(units, 24 + 13 + 12 + 16);
}

/// GREEN: lane resolution is a pure function of the recorded class, and the
/// canonical discriminants round-trip.
#[test]
fn structural_classes_resolve_to_documented_lanes() {
    assert_eq!(
        Lane::for_structural_class(StructuralClass::ExactSpan),
        Lane::Constitutional
    );
    assert_eq!(
        Lane::for_structural_class(StructuralClass::Identifier),
        Lane::Constitutional
    );
    assert_eq!(
        Lane::for_structural_class(StructuralClass::Code),
        Lane::Evidential
    );
    assert_eq!(
        Lane::for_structural_class(StructuralClass::TestLog),
        Lane::Evidential
    );
    assert_eq!(
        Lane::for_structural_class(StructuralClass::Noise),
        Lane::Ephemeral
    );
    assert_eq!(
        Lane::for_structural_class(StructuralClass::Unknown),
        Lane::Decisional
    );
    for code in 1..=6_u64 {
        let class = StructuralClass::from_code(code).unwrap();
        assert_eq!(class.code(), code, "codes round-trip");
        assert!(!class.name().is_empty());
    }
    assert_eq!(StructuralClass::from_code(0), None);
    assert_eq!(StructuralClass::from_code(7), None);
}
