//! Red tests for the context kernel: reducer determinism, IR invariants, legality.

use crate::context_kernel::canonical::Sink;
use crate::context_kernel::events::{
    AppendSource, EventKind, EventLog, LedgerEventKind, OperationClass, ProviderTurnKind,
    RecordedEvent, Sequencer, FIRST_SEQUENCE, GENESIS_CHECKSUM,
};
use crate::context_kernel::ir::{
    covered_units, slice_into, ConversationIr, IrError, Item, ItemId, Region, SegmentClaim,
    SplitContract, SplitNamespace, StoreRange, StructuralClass,
};
use crate::context_kernel::lanes::{
    Lane, LanePolicyRegistry, PolicyError, LANE_POLICY_LATEST_VERSION,
};
use crate::context_kernel::legality::{
    is_legal, QuotingConvention, RenderContract, ToolRole, Violation,
};
use crate::context_kernel::migration::{
    decide, Generation, MigrationDecision, MigrationDescriptor, MigrationPlan, PrivateBuild,
    Publication, PublicationError, SlotPair, V2, V3,
};
use crate::context_kernel::reducer::{
    Reducer, ReducerError, TypedState, IDLENESS_WINDOW, INITIAL_VERSION,
};
use crate::context_kernel::scopes::{ScopeError, ScopeState};

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

fn placed_state() -> TypedState {
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut log = EventLog::new(V2);
    append(
        op(OperationClass::ScopeOpen, 1, 0),
        &mut sequencer,
        &mut log,
    );
    append(user("head", 1), &mut sequencer, &mut log);
    append(user("body", 1), &mut sequencer, &mut log);
    append(
        op(OperationClass::Place, 2, Region::Head.rank()),
        &mut sequencer,
        &mut log,
    );
    append(
        op(OperationClass::Place, 3, Region::Body.rank()),
        &mut sequencer,
        &mut log,
    );
    Reducer::new(IDLENESS_WINDOW).fold(&log).unwrap()
}

fn state_for_legality() -> TypedState {
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut log = EventLog::new(V2);
    append(
        op(OperationClass::ScopeOpen, 1, 0),
        &mut sequencer,
        &mut log,
    );
    append(user("head", 1), &mut sequencer, &mut log);
    append(tool("call-1", "read", 1), &mut sequencer, &mut log);
    append(
        op(OperationClass::Place, 2, Region::Head.rank()),
        &mut sequencer,
        &mut log,
    );
    append(
        op(OperationClass::Place, 3, Region::Body.rank()),
        &mut sequencer,
        &mut log,
    );
    Reducer::new(IDLENESS_WINDOW).fold(&log).unwrap()
}

fn pairing_contract(paired: bool) -> RenderContract {
    let mut contract = RenderContract::generous(1);
    contract.declare("read", "call-1", ToolRole::Call);
    if paired {
        contract.declare("read", "call-1", ToolRole::Result);
    }
    contract
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
    assert_eq!(ids, vec![3, 4], "append events become items in log order");
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
fn migration_crash_matrix_keeps_v2_until_selection_completes() {
    assert_eq!(decide(&EventLog::new(V2)).store_version(), V2);
    assert_eq!(decide(&sample_log(V2)).store_version(), V2);
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut selected = EventLog::new(V2);
    append(
        op(OperationClass::ScopeOpen, 1, 0),
        &mut sequencer,
        &mut selected,
    );
    append(user("a", 1), &mut sequencer, &mut selected);
    let sequence = append(
        op(OperationClass::MigrationSelect, V3, 0),
        &mut sequencer,
        &mut selected,
    )
    .sequence;
    assert_eq!(
        decide(&selected),
        MigrationDecision::SelectV3 {
            selected_sequence: sequence,
        }
    );
    let mut wrong_target = EventLog::new(V2);
    let mut wrong_sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    append(
        op(OperationClass::ScopeOpen, 1, 0),
        &mut wrong_sequencer,
        &mut wrong_target,
    );
    append(user("a", 1), &mut wrong_sequencer, &mut wrong_target);
    append(
        op(OperationClass::MigrationSelect, 9, 0),
        &mut wrong_sequencer,
        &mut wrong_target,
    );
    assert_eq!(decide(&wrong_target).store_version(), V2);
    assert_eq!(decide(&EventLog::new(V3)).store_version(), V3);
}

#[test]
fn migration_publication_requires_a_complete_private_build() {
    let plan = MigrationPlan::from(
        V3,
        vec![StoreRange {
            offset: 0,
            length: 12,
        }],
        77,
    );
    assert_eq!(plan.units(), 12);
    let mut build = PrivateBuild::start(plan);
    assert!(Publication::of(&build).is_none());
    build.complete_with(&[1_u8; 4]);
    let publication = Publication::of(&build).unwrap();
    assert_eq!(publication.store_version, V3);
    assert!(!publication.published);
}

#[test]
fn ir_split_is_claim_atomic() {
    let mut ir = ConversationIr::new();
    let id = ir.reserve_append_id();
    ir.insert(Item::new(
        id,
        Lane::Evidential,
        vec![StoreRange {
            offset: 10,
            length: 9,
        }],
        1,
    ))
    .unwrap();
    let parent_units = ir.item(id).unwrap().units();
    let children = ir
        .split(
            id,
            slice_into(
                &[StoreRange {
                    offset: 10,
                    length: 9,
                }],
                3,
            ),
            &SplitContract {
                namespace: SplitNamespace::Fresh,
                parts: 3,
                split_points: vec![1; 3],
            },
        )
        .unwrap();
    assert_eq!(children.len(), 3);
    assert!(ir.item(id).is_err(), "the parent identifier is retired");
    assert_eq!(
        children[0],
        ItemId::split(0),
        "split children mint from the split namespace, not the append sequence"
    );
    let total: u64 = children
        .iter()
        .map(|child| ir.item(*child).unwrap().units())
        .sum();
    assert_eq!(total, parent_units, "child ranges cover the parent exactly");
    let mut overlapping = ConversationIr::new();
    let other = overlapping.reserve_append_id();
    overlapping
        .insert(Item::new(
            other,
            Lane::Evidential,
            vec![StoreRange {
                offset: 0,
                length: 4,
            }],
            1,
        ))
        .unwrap();
    let bad = overlapping.split(
        other,
        vec![
            vec![StoreRange {
                offset: 0,
                length: 3,
            }],
            vec![StoreRange {
                offset: 2,
                length: 2,
            }],
        ],
        &SplitContract {
            namespace: SplitNamespace::Fresh,
            parts: 2,
            split_points: vec![1, 1],
        },
    );
    assert!(bad.is_err(), "overlapping children are rejected");
}

#[test]
fn ir_slice_partitions_bytes_totally_and_disjointly() {
    let ranges = vec![
        StoreRange {
            offset: 0,
            length: 4,
        },
        StoreRange {
            offset: 4,
            length: 5,
        },
    ];
    let pieces = slice_into(&ranges, 3);
    assert_eq!(pieces.len(), 3);
    let flat: Vec<StoreRange> = pieces.concat();
    assert_eq!(covered_units(&flat), 9, "no byte is counted twice");
    assert_eq!(covered_units(&flat), covered_units(&ranges));
    assert!(slice_into(&ranges, 0).is_empty());
}

#[test]
fn ir_partitions_by_lane() {
    let state = placed_state();
    let mut seen: Vec<u64> = Vec::new();
    let mut counted = 0_u64;
    for lane in Lane::all() {
        for id in state.conversation_ir.items_in_lane(lane) {
            assert!(!seen.contains(&id.value()), "each item has one lane");
            seen.push(id.value());
            counted += state.conversation_ir.item(id).unwrap().units();
        }
    }
    assert_eq!(seen.len(), state.conversation_ir.len());
    assert_eq!(counted, 8, "lane partitions cover every item");
    assert_eq!(state.conversation_ir.lane_units(Lane::Evidential), 0);
}

#[test]
fn ir_charges_each_placed_item_to_one_region() {
    let state = placed_state();
    let head = state.conversation_ir.region_items(Region::Head);
    let body = state.conversation_ir.region_items(Region::Body);
    assert_eq!(head.len(), 1);
    assert_eq!(body.len(), 1);
    assert_ne!(head[0], body[0], "an item sits in one region only");
    let charged: u64 = Region::all()
        .iter()
        .map(|region| state.conversation_ir.region_occupancy(*region))
        .sum();
    assert_eq!(charged, 8, "occupancy is charged exactly once");
    let mut moved = state;
    moved.conversation_ir.place(body[0], Region::Tail).unwrap();
    assert!(moved.conversation_ir.region_items(Region::Body).is_empty());
    assert_eq!(moved.conversation_ir.region_occupancy(Region::Tail), 4);
    moved.conversation_ir.unplace(head[0]).unwrap();
    assert!(moved.conversation_ir.region_items(Region::Head).is_empty());
    assert_eq!(
        moved.conversation_ir.item(head[0]).unwrap().region(),
        None,
        "unplaced items keep their bytes"
    );
}

#[test]
fn ir_identifiers_are_immutable_and_never_reused() {
    let mut ir = ConversationIr::new();
    let id = ir.reserve_append_id();
    ir.insert(Item::new(
        id,
        Lane::Decisional,
        vec![StoreRange {
            offset: 0,
            length: 3,
        }],
        1,
    ))
    .unwrap();
    let duplicate = ir.insert(Item::new(
        id,
        Lane::Decisional,
        vec![StoreRange {
            offset: 0,
            length: 3,
        }],
        1,
    ));
    assert_eq!(
        duplicate.err(),
        Some(crate::context_kernel::ir::IrError::DuplicateItem { id: id.value() })
    );
    let child = ir
        .split(
            id,
            vec![vec![StoreRange {
                offset: 0,
                length: 3,
            }]],
            &SplitContract {
                namespace: SplitNamespace::Fresh,
                parts: 1,
                split_points: vec![1],
            },
        )
        .unwrap();
    assert_eq!(
        child[0].namespace(),
        crate::context_kernel::ir::ItemNamespace::Split,
        "split children mint from the split namespace"
    );
    assert_eq!(child[0].value(), 0, "the split sequence starts at zero");
    assert_ne!(child[0], id, "a split child never equals any append id");
    assert_eq!(
        ir.reserve_append_id(),
        ItemId::append(1),
        "appends continue 1, 2, ... unaware of split ids"
    );
}

#[test]
fn interleaved_appends_and_splits_never_collide() {
    let mut ir = ConversationIr::new();
    let first = ir.reserve_append_id();
    ir.insert(Item::new(
        first,
        Lane::Evidential,
        vec![StoreRange {
            offset: 0,
            length: 4,
        }],
        1,
    ))
    .unwrap();
    let children = ir
        .split(
            first,
            slice_into(
                &[StoreRange {
                    offset: 0,
                    length: 4,
                }],
                2,
            ),
            &SplitContract {
                namespace: SplitNamespace::Fresh,
                parts: 2,
                split_points: vec![1, 1],
            },
        )
        .unwrap();
    assert_eq!(children.len(), 2);
    let second = ir.reserve_append_id();
    assert_ne!(second, first, "append sequence is untouched by split mints");
    ir.insert(Item::new(
        second,
        Lane::Evidential,
        vec![StoreRange {
            offset: 4,
            length: 4,
        }],
        1,
    ))
    .unwrap();
    let ids: std::collections::BTreeSet<ItemId> = ir.items().iter().map(|item| item.id()).collect();
    assert_eq!(
        ids.len(),
        ir.len(),
        "every live item identifier is distinct across namespaces"
    );
    assert_eq!(
        ir.reserve_split_id(),
        ItemId::split(2),
        "the split sequence continues past both minted children"
    );
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
fn legality_rejects_unpaired_tool_interactions() {
    let state = state_for_legality();
    let violation = is_legal(&state, &pairing_contract(false)).unwrap_err();
    assert_eq!(violation.kind(), "pairing");
    assert!(violation.predicate().contains("call-1"));
    assert!(matches!(violation, Violation::Pairing { .. }));
    assert!(is_legal(&state, &pairing_contract(true)).is_ok());
}

#[test]
fn legality_rejects_out_of_order_regions() {
    let mut state = state_for_legality();
    let moved = state.conversation_ir.region_items(Region::Body)[0];
    state.conversation_ir.unplace(moved).unwrap();
    let head = state.conversation_ir.region_items(Region::Head)[0];
    state.conversation_ir.place(head, Region::Tail).unwrap();
    state.conversation_ir.place(moved, Region::Head).unwrap();
    let violation = is_legal(&state, &RenderContract::generous(1)).unwrap_err();
    assert_eq!(violation.kind(), "ordering");
    assert!(matches!(violation, Violation::Ordering { .. }));
}

#[test]
fn legality_rejects_placeholders_the_target_cannot_render() {
    let mut state = state_for_legality();
    let target = state.conversation_ir.region_items(Region::Body)[0];
    let refused = state.conversation_ir.split(
        target,
        vec![vec![], vec![]],
        &SplitContract {
            namespace: SplitNamespace::Fresh,
            parts: 2,
            split_points: vec![1, 1],
        },
    );
    assert!(
        refused.is_err(),
        "an empty part is refused, so collapse must be explicit"
    );
    let mut contract = RenderContract::generous(1);
    assert!(is_legal(&state, &contract).is_ok());
    contract.supports_placeholders = false;
    state
        .conversation_ir
        .insert(Item::phantom(ItemId::split(99), Lane::Evidential, 1))
        .unwrap();
    assert!(state
        .conversation_ir
        .item(ItemId::split(99))
        .unwrap()
        .is_placeholder());
    let violation = is_legal(&state, &contract).unwrap_err();
    assert_eq!(violation.kind(), "placeholder-illegal");
    assert!(matches!(violation, Violation::PlaceholderIllegal { .. }));
}

#[test]
fn legality_rejects_region_over_budget_and_unsupported_regions() {
    let state = state_for_legality();
    let mut contract = RenderContract::generous(1);
    contract.region_budgets = vec![(Region::Head, 1)];
    let violation = is_legal(&state, &contract).unwrap_err();
    assert_eq!(violation.kind(), "region-over-budget");
    assert!(violation.predicate().contains("head"));
    let mut notes = RenderContract::generous(1);
    notes.supports_notes_region = false;
    assert_eq!(notes.region_budget(Region::Notes), 0);
    assert_eq!(notes.region_budget(Region::Head), 1_000_000);
    assert!(
        is_legal(&state, &notes).is_ok(),
        "an empty region is in budget"
    );
}

#[test]
fn legality_rejects_reclamation_below_a_lane_floor() {
    let state = state_for_legality();
    let mut below = state.clone();
    let id = below.conversation_ir.region_items(Region::Head)[0];
    below.conversation_ir.unplace(id).unwrap();
    let violation = is_legal(&below, &RenderContract::generous(1)).unwrap_err();
    assert_eq!(violation.kind(), "floor");
    assert!(matches!(violation, Violation::Floor { .. }));
    let mut zeroed = RenderContract::generous(1);
    zeroed.region_budgets = vec![
        (Region::Head, 0),
        (Region::Notes, 0),
        (Region::Body, 0),
        (Region::Tail, 0),
    ];
    let violation = is_legal(&state, &zeroed).unwrap_err();
    assert_eq!(violation.kind(), "region-over-budget");
}

#[test]
fn legality_protects_pinned_items() {
    let mut state = state_for_legality();
    let pinned = ItemId::new(50);
    state
        .conversation_ir
        .insert(Item::new(
            pinned,
            Lane::Ephemeral,
            vec![StoreRange {
                offset: 90,
                length: 2,
            }],
            1,
        ))
        .unwrap();
    state.conversation_ir.place(pinned, Region::Body).unwrap();
    state.pins.push(pinned);
    assert!(is_legal(&state, &RenderContract::generous(1)).is_ok());
    let mut unplaced = state.clone();
    unplaced.conversation_ir.unplace(pinned).unwrap();
    let violation = is_legal(&unplaced, &RenderContract::generous(1)).unwrap_err();
    assert_eq!(violation.kind(), "pin");
    assert!(violation.predicate().contains("unplaced"));
    let mut absent = state;
    absent.pins = vec![ItemId::new(4242)];
    let violation = is_legal(&absent, &RenderContract::generous(1)).unwrap_err();
    assert_eq!(violation.kind(), "pin");
}

#[test]
fn legality_enforces_the_quoting_convention() {
    let state = state_for_legality();
    let mut contract = RenderContract::generous(1);
    contract.quoting_convention = QuotingConvention::XmlDelimited;
    let violation = is_legal(&state, &contract).unwrap_err();
    assert_eq!(violation.kind(), "quoting-convention");
    assert!(matches!(violation, Violation::QuotingConvention { .. }));
    assert_eq!(QuotingConvention::Fenced.name(), "fenced");
}

#[test]
fn legality_table_covers_all_seven_predicates() {
    let names: Vec<&str> = crate::context_kernel::legality::rules()
        .iter()
        .map(|rule| rule.name)
        .collect();
    assert_eq!(
        names,
        vec![
            "pairing",
            "ordering",
            "placeholder-illegal",
            "region-over-budget",
            "floor",
            "pin",
            "quoting-convention",
        ]
    );
}

#[test]
fn legality_contract_version_must_match_between_commit_and_send() {
    let state = state_for_legality();
    let committed = is_legal(&state, &RenderContract::generous(4)).unwrap();
    assert_eq!(committed.contract_version(), 4);
    assert!(committed.sendable_with(4));
    assert!(!committed.sendable_with(5), "a stale contract cannot send");
}

#[test]
fn migration_publication_swaps_only_after_a_complete_build() {
    let chain = crate::context_kernel::canonical::HashScope::EventChain.digest(b"v2 events");
    let mut slots = SlotPair::genesis(V2, 40, chain);
    assert_eq!(slots.active().store_version(), V2);
    assert!(slots.inactive().is_none());
    assert!(!slots.published());

    let swap_without_build = slots.swap(chain).unwrap_err();
    assert_eq!(swap_without_build, PublicationError::NoBuildPending);

    let committed = Generation::Committed {
        store_version: V2,
        bytes: 40,
        chain,
    };
    let land_committed = slots.land(committed).unwrap_err();
    assert_eq!(
        land_committed,
        PublicationError::SlotContract {
            expected: crate::context_kernel::canonical::HashScope::StoreBuild,
            found: crate::context_kernel::canonical::HashScope::EventChain,
        },
        "a committed generation never lands in the build slot"
    );

    let built_bytes = [7_u8; 40];
    let build = Generation::Built {
        store_version: V3,
        bytes: built_bytes.len() as u64,
        checksum: crate::context_kernel::canonical::HashScope::StoreBuild.digest(&built_bytes),
    };
    slots.land(build).unwrap();
    assert_eq!(
        slots.land(build).unwrap_err(),
        PublicationError::BuildPending,
        "the inactive slot holds one build at a time"
    );
    assert_eq!(slots.active().store_version(), V2, "landing is invisible");

    slots.discard().unwrap();
    assert!(slots.inactive().is_none());
    slots.land(build).unwrap();

    let selection_chain =
        crate::context_kernel::canonical::HashScope::EventChain.chain(chain, b"select v3");
    let published = slots.swap(selection_chain).unwrap();
    assert_eq!(published.store_version(), V3);
    assert_eq!(slots.active().store_version(), V3, "the swap is the switch");
    assert!(slots.published());
    assert!(matches!(
        slots.inactive(),
        Some(Generation::Committed { .. })
    ));
    assert_eq!(
        slots.swap(selection_chain).unwrap_err(),
        PublicationError::AlreadyPublished,
        "a publication happens at most once"
    );

    let descriptor = MigrationDescriptor::seal(
        V3,
        crate::context_kernel::canonical::HashScope::StoreBuild.digest(&built_bytes),
        selection_chain,
    );
    assert!(descriptor.verify_build(&built_bytes));
    let mut tampered = built_bytes;
    tampered[0] ^= 1;
    assert!(!descriptor.verify_build(&tampered));

    let mut log = EventLog::new(V2);
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    append(
        op(OperationClass::ScopeOpen, 1, 0),
        &mut sequencer,
        &mut log,
    );
    let selection = append(
        op(OperationClass::MigrationSelect, V3, 0),
        &mut sequencer,
        &mut log,
    );
    let mut verified = log.clone();
    append(
        op(OperationClass::StoreMode, 2, 0),
        &mut sequencer,
        &mut verified,
    );
    assert!(
        !descriptor.verify_chain(&log),
        "chain precedes the selection"
    );
    let descriptor = MigrationDescriptor::seal(
        V3,
        crate::context_kernel::canonical::HashScope::StoreBuild.digest(&built_bytes),
        verified.head_checksum(),
    );
    assert!(descriptor.verify_chain(&verified));
    assert_eq!(selection.store_version, V2);

    let mut sink = Sink::new();
    descriptor.encode(&mut sink);
    let encoded = sink.finish();
    let mut sink_again = Sink::new();
    descriptor.encode(&mut sink_again);
    assert_eq!(encoded, sink_again.finish());
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

#[test]
fn store_only_and_phantom_are_distinct_queryable_states() {
    let mut ir = ConversationIr::new();
    let id = ir.reserve_append_id();
    ir.insert(Item::store_only(
        id,
        Lane::Evidential,
        vec![StoreRange {
            offset: 0,
            length: 5,
        }],
        1,
    ))
    .unwrap();
    assert!(ir.item(id).unwrap().is_store_only());
    assert_eq!(ir.item(id).unwrap().units(), 5, "store-only keeps bytes");
    assert_eq!(ir.region_occupancy(Region::Body), 0);
    assert_eq!(
        ir.region_items(Region::Body),
        Vec::new(),
        "store-only sits out of every region"
    );

    ir.place(id, Region::Body).unwrap();
    assert!(ir.item(id).unwrap().region() == Some(Region::Body));
    assert_eq!(ir.region_occupancy(Region::Body), 5);

    ir.unplace(id).unwrap();
    assert!(
        ir.item(id).unwrap().is_store_only(),
        "unplace is store-only"
    );

    let phantom_id = ir.reserve_split_id();
    ir.insert(Item::phantom(phantom_id, Lane::Ephemeral, 1))
        .unwrap();
    assert!(ir.item(phantom_id).unwrap().is_placeholder());
    assert_eq!(ir.item(phantom_id).unwrap().units(), 0);
    assert_eq!(
        ir.collapse(phantom_id).unwrap_err(),
        crate::context_kernel::ir::IrError::PlacementState {
            id: phantom_id.value()
        },
        "a phantom never collapses twice"
    );

    let collapse_id = ir.reserve_append_id();
    ir.insert(Item::new(
        collapse_id,
        Lane::Decisional,
        vec![StoreRange {
            offset: 9,
            length: 4,
        }],
        1,
    ))
    .unwrap();
    ir.collapse(collapse_id).unwrap();
    let collapsed = ir.item(collapse_id).unwrap();
    assert!(collapsed.is_placeholder());
    assert_eq!(collapsed.units(), 0, "collapse clears the bytes");
    assert_eq!(
        ir.region_occupancy(Region::Body),
        0,
        "an unplaced item was never charged, and its collapsed phantom charges nothing"
    );
}

#[test]
fn split_never_cuts_through_recorded_claim_boundaries() {
    let mut ir = ConversationIr::new();
    let id = ir.reserve_append_id();
    let provenance = vec![
        StoreRange {
            offset: 0,
            length: 4,
        },
        StoreRange {
            offset: 10,
            length: 6,
        },
    ];
    ir.insert(Item::new(id, Lane::Evidential, provenance.clone(), 1))
        .unwrap();

    let refused = ir.split(
        id,
        vec![provenance.clone()],
        &SplitContract {
            namespace: SplitNamespace::Fresh,
            parts: 1,
            split_points: vec![2],
        },
    );
    assert_eq!(
        refused.unwrap_err(),
        crate::context_kernel::ir::IrError::ClaimBoundary { id: id.value() },
        "one part over two claims would have to cut a claim"
    );

    let grouped = vec![vec![provenance[0]], vec![provenance[1]]];
    let children = ir
        .split(
            id,
            grouped,
            &SplitContract {
                namespace: SplitNamespace::Fresh,
                parts: 2,
                split_points: vec![1, 1],
            },
        )
        .unwrap();
    assert_eq!(
        children.len(),
        2,
        "claims group whole, so parts cannot exceed claims"
    );
    let first = ir.item(children[0]).unwrap();
    let second = ir.item(children[1]).unwrap();
    assert_eq!(
        first.provenance,
        vec![StoreRange {
            offset: 0,
            length: 4,
        }],
        "the first claim stays whole"
    );
    assert_eq!(
        second.provenance,
        vec![StoreRange {
            offset: 10,
            length: 6,
        }],
        "the second claim stays whole"
    );
    let total: u64 = children
        .iter()
        .map(|child| ir.item(*child).unwrap().units())
        .sum();
    assert_eq!(total, 10, "coverage is preserved across the regroup");
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
    assert_eq!(state.conversation_ir.items()[0].id(), ItemId::append(2));
}

/// GREEN: claims that do not cover the append are refused, not silently re-covered.
#[test]
fn partial_claims_are_refused() {
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut log = EventLog::new(V2);
    append(
        op(OperationClass::ScopeOpen, 1, 0),
        &mut sequencer,
        &mut log,
    );
    let sequence = append(
        EventKind::Append {
            source: AppendSource::User,
            sanitized: b"twenty four bytes of payload".to_vec(),
            scope: 1,
            claims: vec![SegmentClaim {
                span: StoreRange {
                    offset: 0,
                    length: 5,
                },
                class: None,
            }],
        },
        &mut sequencer,
        &mut log,
    )
    .sequence;
    assert_eq!(
        Reducer::new(IDLENESS_WINDOW).fold(&log).unwrap_err(),
        ReducerError::Ir(IrError::ClaimsDontCover { sequence }),
        "a claim list that drops bytes is a typed refusal"
    );
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
