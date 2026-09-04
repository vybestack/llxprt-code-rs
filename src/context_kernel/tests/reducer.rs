//! Reducer and event-log tests: total order, dedup, replay, refusals, resegment.

use crate::context_kernel::canonical::Sink;
use crate::context_kernel::events::{
    AppendSource, EventKind, EventLog, LedgerEventKind, OperationClass, ProviderTurnKind,
    RecordedEvent, Sequencer, FIRST_SEQUENCE, GENESIS_CHECKSUM,
};
use crate::context_kernel::ir::{IrError, ItemId, ItemNamespace, SegmentClaim, StoreRange};
use crate::context_kernel::lanes::Lane;
use crate::context_kernel::migration::{V2, V3};
use crate::context_kernel::reducer::{
    Reducer, ReducerError, TypedState, IDLENESS_WINDOW, INITIAL_VERSION,
};

fn encode_state(state: &TypedState) -> Vec<u8> {
    let mut sink = Sink::new();
    state.encode(&mut sink);
    sink.finish()
}

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

fn tool(call_id: &str, tool: &str, scope: u64) -> EventKind {
    EventKind::Append {
        source: AppendSource::ToolResult {
            call_id: String::from(call_id),
            tool: String::from(tool),
        },
        sanitized: vec![7_u8; 6],
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

fn sample_log(store_version: u64) -> EventLog {
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut log = EventLog::new(store_version);
    append(
        op(OperationClass::ScopeOpen, 1, 0),
        &mut sequencer,
        &mut log,
    );
    append(
        EventKind::Ledger {
            kind: LedgerEventKind::ObligationAdmitted,
            key: String::from("obligation-1"),
        },
        &mut sequencer,
        &mut log,
    );
    append(user("task", 1), &mut sequencer, &mut log);
    append(tool("call-1", "read", 1), &mut sequencer, &mut log);
    append(
        EventKind::ProviderTurn {
            kind: ProviderTurnKind::Conversation,
            request_units: 128,
        },
        &mut sequencer,
        &mut log,
    );
    log
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
fn reducer_folds_total_order() {
    let log = sample_log(V2);
    let state = Reducer::new(IDLENESS_WINDOW).fold(&log).unwrap();
    assert_eq!(state.applied_len(), log.len());
    let ids: Vec<u64> = state
        .conversation_ir
        .items()
        .iter()
        .map(|item| item.id().value())
        .collect();
    assert_eq!(
        ids,
        vec![0, 1],
        "append events become items in log order, minted by the append counter"
    );
    assert_eq!(state.conversation_ir.items()[0].lane, Lane::Constitutional);
    assert_eq!(state.conversation_ir.items()[1].lane, Lane::Evidential);
}

#[test]
fn reducer_deduplicates_by_event_identity() {
    let log = sample_log(V2);
    let reducer = Reducer::new(IDLENESS_WINDOW);
    let mut state = reducer.fold(&log).unwrap();
    let hash = state.state_hash;
    for event in log.events() {
        assert!(state.is_applied(event.sequence, event.body_digest));
    }
    reducer.fold_from(&mut state, &log).unwrap();
    assert_eq!(
        state.applied_len(),
        log.len(),
        "replayed identities are skipped"
    );
    assert_eq!(state.state_hash, hash, "dedup leaves state untouched");
    let prefix = log.prefix(2);
    reducer.fold_from(&mut state, &prefix).unwrap();
    assert_eq!(state.applied_len(), log.len());
    assert_eq!(state.state_hash, hash);
}

#[test]
fn reducer_replay_of_prefix_is_byte_identical() {
    let log = sample_log(V2);
    let reducer = Reducer::new(IDLENESS_WINDOW);
    let full = reducer.fold(&log).unwrap();
    let first_pass = reducer.fold(&log.prefix(3)).unwrap();
    let second_pass = reducer.fold(&log.prefix(3)).unwrap();
    assert_eq!(encode_state(&first_pass), encode_state(&second_pass));
    let mut resumed = first_pass;
    reducer.fold_from(&mut resumed, &log).unwrap();
    assert_eq!(
        encode_state(&resumed),
        encode_state(&full),
        "resuming a folded prefix is byte-identical to one pass"
    );
    assert_eq!(resumed.state_hash, full.state_hash);
    assert_eq!(resumed.conversation_ir, full.conversation_ir);
    assert_eq!(resumed.scope_registry, full.scope_registry);
    let clock = log.replay_clock();
    assert_eq!(clock.recorded_unix_ms(1), Some(1_000));
    assert_eq!(clock.recorded_unix_ms(999), None);
    assert_eq!(clock.last_recorded_unix_ms(), 1_000);
}

#[test]
fn replay_uses_recorded_time_only() {
    let mut log = EventLog::new(V2);
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 5_000);
    append(
        op(OperationClass::ScopeOpen, 1, 0),
        &mut sequencer,
        &mut log,
    );
    append(user("a", 1), &mut sequencer, &mut log);
    let second = sequencer.append_at(user("b", 1), V2, 9_000);
    log.append(second).unwrap();
    let mut later = Sequencer::new(FIRST_SEQUENCE, 1, 500_000);
    let mut replayed = EventLog::new(V2);
    for event in log.events() {
        let clone = later.append_at(event.kind.clone(), V2, event.recorded_unix_ms);
        replayed.append(clone).unwrap();
    }
    let reducer = Reducer::new(IDLENESS_WINDOW);
    assert_eq!(
        reducer.fold(&log).unwrap().state_hash,
        reducer.fold(&replayed).unwrap().state_hash,
        "a live clock never enters the reducer"
    );
}

#[test]
fn event_log_rejects_gaps_duplicates_and_bad_checksums() {
    let log = sample_log(V2);
    let mut other = EventLog::new(V2);
    let result = other.append(log.events()[2].clone());
    assert_eq!(
        result.err(),
        Some(crate::context_kernel::events::LogError::SequenceGap {
            expected: FIRST_SEQUENCE,
            actual: 3,
        })
    );
    let mut duplicate = log.prefix(3);
    assert!(duplicate.append(log.events()[2].clone()).is_err());
    let mut tampered = log.prefix(2);
    let mut broken = log.events()[2].clone();
    broken.checksum ^= 1;
    assert!(tampered.append(broken).is_err());
    let mut versioned = log.prefix(2);
    let mut wrong_store = log.events()[2].clone();
    wrong_store.store_version = V3;
    assert!(versioned.append(wrong_store).is_err());
    assert_eq!(log.head_checksum(), sample_log(V2).head_checksum());
    assert_ne!(log.head_checksum(), GENESIS_CHECKSUM);
}

#[test]
fn reducer_detects_version_conflicts() {
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut log = EventLog::new(V2);
    append(
        op(OperationClass::ScopeOpen, 1, 0),
        &mut sequencer,
        &mut log,
    );
    append(user("a", 1), &mut sequencer, &mut log);
    let bad = append(
        op(OperationClass::LanePolicyUpdate, 2, 7),
        &mut sequencer,
        &mut log,
    );
    assert_eq!(
        Reducer::new(IDLENESS_WINDOW).fold(&log).unwrap_err(),
        ReducerError::VersionConflict {
            claimed_parent: 7,
            actual: INITIAL_VERSION,
        }
    );
    assert_eq!(bad.sequence, 3);
}

#[test]
fn reducer_applies_lane_policy_update_by_compare_and_commit() {
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut log = EventLog::new(V2);
    append(
        op(OperationClass::ScopeOpen, 1, 0),
        &mut sequencer,
        &mut log,
    );
    append(user("a", 1), &mut sequencer, &mut log);
    append(
        op(OperationClass::LanePolicyUpdate, 2, INITIAL_VERSION),
        &mut sequencer,
        &mut log,
    );
    let state = Reducer::new(IDLENESS_WINDOW).fold(&log).unwrap();
    assert_eq!(state.version, 2);
    assert_eq!(state.lane_policy_registry.version(), 2);
}

#[test]
fn reducer_rejects_unknown_regions() {
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut log = EventLog::new(V2);
    append(
        op(OperationClass::ScopeOpen, 1, 0),
        &mut sequencer,
        &mut log,
    );
    append(user("a", 1), &mut sequencer, &mut log);
    append(op(OperationClass::Place, 1, 9), &mut sequencer, &mut log);
    assert_eq!(
        Reducer::new(IDLENESS_WINDOW).fold(&log).unwrap_err(),
        ReducerError::UnknownRegion { rank: 9 }
    );
}

#[test]
fn resumed_sequencer_continues_the_recorded_chain() {
    let mut log = EventLog::new(V2);
    let mut live = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    append(op(OperationClass::ScopeOpen, 1, 0), &mut live, &mut log);
    append(user("before the restart", 1), &mut live, &mut log);
    let prefix = log.prefix(log.len());

    let mut resumed = Sequencer::resume(
        prefix.len() as u64 + FIRST_SEQUENCE,
        prefix.head_checksum(),
        1,
        2_000,
    );
    let event = resumed.append(user("after the restart", 1), V2);
    assert_eq!(event.sequence, 3, "the sequence continues the prefix");
    assert_eq!(
        log.append(event.clone()).unwrap(),
        3,
        "the resumed event verifies against the recorded chain"
    );

    let mut from_genesis = Sequencer::new(4, 1, 2_000);
    let broken = from_genesis.append(user("after the restart", 1), V2);
    assert_eq!(broken.sequence, 4);
    assert_eq!(
        log.append(broken).unwrap_err(),
        crate::context_kernel::events::LogError::ChecksumMismatch { sequence: 4 },
        "a genesis-restarted chain is refused by the log"
    );

    let continued = Sequencer::continuing(&log, 1, 3_000);
    assert_eq!(continued.next_sequence(), 4);
    assert_eq!(continued.last_checksum(), log.head_checksum());

    let mut continued = continued;
    let tail = continued.append(user("tail", 1), V2);
    log.append(tail.clone()).unwrap();
    let mut replay = Sequencer::new(FIRST_SEQUENCE, 1, 0);
    let mut verified = EventLog::new(V2);
    for recorded in log.events() {
        let clone = replay.append_at(recorded.kind.clone(), V2, recorded.recorded_unix_ms);
        verified.append(clone).unwrap();
    }
    assert_eq!(verified.head_checksum(), log.head_checksum());
    // The chain verifies event by event from genesis through the resume point.
    let mut previous = GENESIS_CHECKSUM;
    for event in log.events() {
        assert!(
            event.verify(previous),
            "sequence {} commits to its recorded predecessor",
            event.sequence
        );
        previous = event.checksum;
    }
    assert_eq!(previous, log.head_checksum());
    assert_eq!(
        Reducer::new(IDLENESS_WINDOW).fold(&log).unwrap().state_hash,
        Reducer::new(IDLENESS_WINDOW)
            .fold(&verified)
            .unwrap()
            .state_hash,
        "the resumed chain replays identically from genesis"
    );
}

#[test]
fn reducer_refuses_appends_to_unopened_scopes() {
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut log = EventLog::new(V2);
    append(user("no scope open yet", 1), &mut sequencer, &mut log);
    assert_eq!(
        Reducer::new(IDLENESS_WINDOW).fold(&log).unwrap_err(),
        ReducerError::UnknownScope { id: 1 },
        "only a logged scope-open creates a scope; the reducer never invents one"
    );
    assert_ne!(
        Reducer::new(IDLENESS_WINDOW).fold(&log).unwrap_err(),
        ReducerError::Scope(crate::context_kernel::scopes::ScopeError::UnknownScope { id: 2 })
    );
}

/// GREEN: a fold that refuses one claim of an append leaves the state untouched:
/// no item of that append lands, not even the claims that minted cleanly. The
/// refused append is one whose second claim cannot become an item, so the refusal
/// happens after the first claim minted.
#[test]
fn a_refused_append_leaves_the_state_untouched() {
    let payload = b"0123456789abcdefghij"; // 20 bytes
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut log = EventLog::new(V2);
    append(
        op(OperationClass::ScopeOpen, 1, 0),
        &mut sequencer,
        &mut log,
    );
    append(user("landed", 1), &mut sequencer, &mut log);
    let before = Reducer::new(IDLENESS_WINDOW).fold(&log).unwrap();
    assert_eq!(before.conversation_ir.len(), 1);

    // An append whose second claim names an append-namespace id over no bytes, so
    // the claim cannot become an item: the append is refused on its second claim
    // while its first claim minted cleanly.
    let mut refused = EventKind::Append {
        source: AppendSource::User,
        sanitized: payload.to_vec(),
        scope: 1,
        claims: contiguous(payload, &[10, 10]),
    };
    if let EventKind::Append { claims, .. } = &mut refused {
        claims[1].span.length = 0;
    }
    let refused_event = sequencer.append(refused, log.store_version());
    log.append(refused_event).unwrap();

    let error = Reducer::new(IDLENESS_WINDOW).fold(&log).unwrap_err();
    assert!(
        matches!(error, ReducerError::Ir(IrError::ClaimsDontCover { .. })),
        "the refusal is the claim list, not a different defect: {error:?}"
    );
    // The state the refused append reached is byte-identical to the state before.
    let untouched = Reducer::new(IDLENESS_WINDOW).fold(&log.prefix(2)).unwrap();
    assert_eq!(
        encode_state(&untouched),
        encode_state(&before),
        "a refused append leaves no partially applied claims behind"
    );
    assert_eq!(
        untouched.conversation_ir.len(),
        before.conversation_ir.len(),
        "no claim of the refused append landed"
    );
    assert_eq!(
        untouched
            .conversation_ir
            .namespace_watermark(ItemNamespace::Append),
        before
            .conversation_ir
            .namespace_watermark(ItemNamespace::Append),
        "the append watermark is not spent by a refused append"
    );
}

/// GREEN: a logged resegment that would cut a recorded claim in two is refused,
/// children, and the children's provenance is disjoint and total.
#[test]
fn reducer_resegment_refines_a_single_claim_into_atomic_children() {
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut log = EventLog::new(V2);
    append(
        op(OperationClass::ScopeOpen, 1, 0),
        &mut sequencer,
        &mut log,
    );
    let payload = b"0123456789abcdefghijklmn"; // 24 bytes
    append(
        user(std::str::from_utf8(payload).unwrap(), 1),
        &mut sequencer,
        &mut log,
    );
    // The first append mints the first append identifier.
    let item = 0;
    append(
        op(OperationClass::Resegment, item, 3),
        &mut sequencer,
        &mut log,
    );
    let state = Reducer::new(IDLENESS_WINDOW).fold(&log).unwrap();
    assert_eq!(
        state.conversation_ir.len(),
        3,
        "three parts, each its own new claim boundary"
    );
    let total: u64 = state
        .conversation_ir
        .items()
        .iter()
        .map(|item| item.units())
        .sum();
    assert_eq!(total as usize, payload.len());
    for child in state.conversation_ir.items() {
        assert_eq!(
            child.provenance.len(),
            1,
            "a refined part is one contiguous claim"
        );
        assert_eq!(
            child.id().namespace(),
            ItemNamespace::Split,
            "logged resegment mints split-namespace children"
        );
    }
    let mut spans: Vec<StoreRange> = state
        .conversation_ir
        .items()
        .iter()
        .map(|item| item.provenance[0])
        .collect();
    spans.sort_by_key(|range| range.offset);
    assert_eq!(
        spans,
        vec![
            StoreRange {
                offset: 0,
                length: 8
            },
            StoreRange {
                offset: 8,
                length: 8
            },
            StoreRange {
                offset: 16,
                length: 8
            },
        ],
        "the parts are disjoint, ordered, and cover the parent exactly"
    );
}

/// GREEN: a logged resegment naming an item that does not exist is a typed
/// refusal, not a panic.
#[test]
fn reducer_resegment_names_a_missing_item_as_a_typed_refusal() {
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut log = EventLog::new(V2);
    append(
        op(OperationClass::ScopeOpen, 1, 0),
        &mut sequencer,
        &mut log,
    );
    append(
        op(OperationClass::Resegment, 99, 2),
        &mut sequencer,
        &mut log,
    );
    assert_eq!(
        Reducer::new(IDLENESS_WINDOW).fold(&log).unwrap_err(),
        ReducerError::Ir(IrError::UnknownItem { id: 99 }),
        "a resegment over a retired or absent item is typed"
    );
}

/// GREEN: interleaved appends and logged resegments never mint a colliding
/// identifier, across both namespaces, from the log alone.
#[test]
fn interleaved_appends_and_logged_resegments_never_collide() {
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut log = EventLog::new(V2);
    append(
        op(OperationClass::ScopeOpen, 1, 0),
        &mut sequencer,
        &mut log,
    );
    // Subjects are item identifiers, which the append mint assigns in log order:
    // 0, 1, 2 for the three appends, independent of the sequences they carry.
    let first = 0;
    append(user("first append", 1), &mut sequencer, &mut log);
    append(
        op(OperationClass::Resegment, first, 2),
        &mut sequencer,
        &mut log,
    );
    let second = 1;
    append(user("second append", 1), &mut sequencer, &mut log);
    append(
        op(OperationClass::Resegment, second, 2),
        &mut sequencer,
        &mut log,
    );
    append(user("third append", 1), &mut sequencer, &mut log);
    let state = Reducer::new(IDLENESS_WINDOW).fold(&log).unwrap();
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
        "every live identifier is distinct across namespaces"
    );
    assert_eq!(state.conversation_ir.len(), 5, "two splits and one append");
    let append_ids: Vec<u64> = ids
        .iter()
        .filter(|id| id.namespace() == ItemNamespace::Append)
        .map(|id| id.value())
        .collect();
    assert_eq!(
        append_ids,
        vec![2],
        "the surviving append identifier is untouched by split mints"
    );
    let split_ids: Vec<u64> = ids
        .iter()
        .filter(|id| id.namespace() == ItemNamespace::Split)
        .map(|id| id.value())
        .collect();
    assert_eq!(
        split_ids,
        vec![0, 1, 2, 3],
        "split children mint from their own sequence, one per child, never reused"
    );
}

/// GREEN: a log containing a migration replays to identical state, and the
/// selection never rewrites the log's framing version, so a v2 log with a v3
/// selection followed by more v2-framed events replays exactly as written.
#[test]
fn a_log_crossing_a_migration_replays_identically() {
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut log = EventLog::new(V2);
    append(
        op(OperationClass::ScopeOpen, 1, 0),
        &mut sequencer,
        &mut log,
    );
    append(user("before the migration", 1), &mut sequencer, &mut log);
    let selection = append(
        op(OperationClass::MigrationSelect, V3, 0),
        &mut sequencer,
        &mut log,
    )
    .sequence;
    append(user("after the migration", 1), &mut sequencer, &mut log);
    append(
        op(OperationClass::StoreMode, 2, 0),
        &mut sequencer,
        &mut log,
    );

    let reducer = Reducer::new(IDLENESS_WINDOW);
    let state = reducer.fold(&log).unwrap();
    assert_eq!(state.store_version, V2, "the log keeps its framing version");
    assert_eq!(
        state.selected_store_version,
        Some(V3),
        "the selection is its own recorded field"
    );
    assert_eq!(selection, 3, "the selection is an event in the total order");

    let first_pass = reducer.fold(&log).unwrap();
    let second_pass = reducer.fold(&log).unwrap();
    assert_eq!(
        encode_state(&first_pass),
        encode_state(&second_pass),
        "two replays of the same log are byte-identical"
    );
    assert_eq!(first_pass.state_hash, second_pass.state_hash);

    for count in 1..=log.len() {
        let prefix = reducer.fold(&log.prefix(count)).unwrap();
        let mut resumed = prefix;
        reducer.fold_from(&mut resumed, &log).unwrap();
        assert_eq!(
            encode_state(&resumed),
            encode_state(&state),
            "every prefix resumes to the identical full state, including the selection"
        );
    }

    let mut other = EventLog::new(V2);
    let mut replay_sequencer = Sequencer::new(FIRST_SEQUENCE, 7, 500_000);
    for recorded in log.events() {
        let clone =
            replay_sequencer.append_at(recorded.kind.clone(), V2, recorded.recorded_unix_ms);
        other.append(clone).unwrap();
    }
    assert_eq!(
        reducer.fold(&other).unwrap().state_hash,
        state.state_hash,
        "replay from a different epoch and clock is identical"
    );
}

/// GREEN: every event records the version it was written under, and the reducer
/// binds each event to the framing version, so a log that crosses a version
/// boundary is refused rather than replayed against the wrong one.
#[test]
fn every_event_binds_the_version_it_was_written_under() {
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut log = EventLog::new(V2);
    append(
        op(OperationClass::ScopeOpen, 1, 0),
        &mut sequencer,
        &mut log,
    );
    append(user("a", 1), &mut sequencer, &mut log);
    append(
        op(OperationClass::MigrationSelect, V3, 0),
        &mut sequencer,
        &mut log,
    );

    // Every recorded event carries the version it was written under.
    for event in log.events() {
        assert_eq!(event.store_version, V2, "each event records its version");
    }

    // An event written under another version is refused by the log.
    let foreign = sequencer.append(user("v3 bytes", 1), V3);
    assert_eq!(
        log.append(foreign).unwrap_err(),
        crate::context_kernel::events::LogError::StoreVersion {
            sequence: 4,
            log: V2,
            event: V3
        },
        "a v3-framed event never lands in a v2 log"
    );

    // And the reducer refuses to fold a version-crossing state.
    let mut state = Reducer::new(IDLENESS_WINDOW).fold(&log).unwrap();
    assert_eq!(state.store_version, V2);
    let mut v3_log = EventLog::new(V3);
    let mut v3_sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let v3_event = v3_sequencer.append(user("v3 bytes", 1), V3);
    v3_log.append(v3_event).unwrap();
    assert_eq!(
        Reducer::new(IDLENESS_WINDOW)
            .fold_from(&mut state, &v3_log)
            .unwrap_err(),
        ReducerError::StoreVersion {
            state: V2,
            event: V3
        },
        "replay binds each event to the version of the state it folds into"
    );
}
