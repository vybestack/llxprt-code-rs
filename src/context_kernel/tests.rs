//! Red tests for the context kernel: reducer determinism, IR invariants, legality.

use crate::context_kernel::canonical::Sink;
use crate::context_kernel::events::{
    AppendSource, EventKind, EventLog, LedgerEventKind, OperationClass, ProviderTurnKind,
    RecordedEvent, Sequencer, FIRST_SEQUENCE, GENESIS_CHECKSUM,
};
use crate::context_kernel::ir::{
    covered_units, slice_into, ConversationIr, Item, ItemId, Region, StoreRange,
};
use crate::context_kernel::lanes::{
    Lane, LanePolicyRegistry, PolicyError, LANE_POLICY_LATEST_VERSION,
};
use crate::context_kernel::legality::{
    is_legal, QuotingConvention, RenderContract, ToolRole, Violation,
};
use crate::context_kernel::migration::{
    decide, MigrationDecision, MigrationPlan, PrivateBuild, Publication, V2, V3,
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
    log.append(event.clone()).ok();
    event
}

fn user(text: &str, scope: u64) -> EventKind {
    EventKind::Append {
        source: AppendSource::User,
        sanitized: text.as_bytes().to_vec(),
        scope,
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
    append(user("head", 1), &mut sequencer, &mut log);
    append(user("body", 1), &mut sequencer, &mut log);
    append(
        op(OperationClass::Place, 1, Region::Head.rank()),
        &mut sequencer,
        &mut log,
    );
    append(
        op(OperationClass::Place, 2, Region::Body.rank()),
        &mut sequencer,
        &mut log,
    );
    Reducer::new(IDLENESS_WINDOW).fold(&log).unwrap()
}

fn state_for_legality() -> TypedState {
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut log = EventLog::new(V2);
    append(user("head", 1), &mut sequencer, &mut log);
    append(tool("call-1", "read", 1), &mut sequencer, &mut log);
    append(
        op(OperationClass::Place, 1, Region::Head.rank()),
        &mut sequencer,
        &mut log,
    );
    append(
        op(OperationClass::Place, 2, Region::Body.rank()),
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
    assert_eq!(bad.sequence, 2);
}

#[test]
fn reducer_applies_lane_policy_update_by_compare_and_commit() {
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut log = EventLog::new(V2);
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
    append(user("a", 1), &mut sequencer, &mut wrong_target);
    append(
        op(OperationClass::MigrationSelect, 9, 0),
        &mut sequencer,
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
    let id = ir.reserve_id();
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
        )
        .unwrap();
    assert_eq!(children.len(), 3);
    assert!(ir.item(id).is_err(), "the parent identifier is retired");
    let total: u64 = children
        .iter()
        .map(|child| ir.item(*child).unwrap().units())
        .sum();
    assert_eq!(total, parent_units, "child ranges cover the parent exactly");
    let mut overlapping = ConversationIr::new();
    let other = overlapping.reserve_id();
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
    let id = ir.reserve_id();
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
    let duplicate = ir.insert(Item::new(id, Lane::Decisional, Vec::new(), 1));
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
        )
        .unwrap();
    assert_eq!(child[0].value(), 1, "split mints a fresh identifier");
    assert!(ir.reserve_id().value() > child[0].value());
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
    let refused = state.conversation_ir.split(target, vec![vec![], vec![]]);
    assert!(
        refused.is_err(),
        "an empty part is refused, so collapse must be explicit"
    );
    let mut contract = RenderContract::generous(1);
    assert!(is_legal(&state, &contract).is_ok());
    contract.supports_placeholders = false;
    state
        .conversation_ir
        .insert(Item::new(ItemId::new(99), Lane::Evidential, Vec::new(), 1))
        .unwrap();
    assert!(state
        .conversation_ir
        .item(ItemId::new(99))
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
