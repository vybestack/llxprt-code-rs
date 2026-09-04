//! Legality-table tests: pairing, ordering, placeholders, budgets, floors, pinning.

use crate::context_kernel::events::{
    AppendSource, EventKind, EventLog, OperationClass, RecordedEvent, Sequencer, FIRST_SEQUENCE,
};
use crate::context_kernel::ir::{Item, ItemId, Region, SplitContract, SplitNamespace, StoreRange};
use crate::context_kernel::lanes::Lane;
use crate::context_kernel::legality::{
    is_legal, QuotingConvention, RenderContract, ToolRole, Violation,
};
use crate::context_kernel::migration::V2;
use crate::context_kernel::reducer::{Reducer, TypedState, IDLENESS_WINDOW};

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

fn state_for_legality() -> TypedState {
    placed_log_items(tool("call-1", "read", 1)).0
}

fn pairing_contract(paired: bool) -> RenderContract {
    let mut contract = RenderContract::generous(1);
    contract.declare("read", "call-1", ToolRole::Call);
    if paired {
        contract.declare("read", "call-1", ToolRole::Result);
    }
    contract
}

/// Builds a state with two placed items and returns it with their identifiers. The
/// subject an operation names is an item identifier, which the IR's append mint
/// assigns in the order the log appends; the helper reports the minted values so
/// the log's event sequences and the items' identifiers stay independent.
fn placed_log_items(second: EventKind) -> (TypedState, ItemId, ItemId) {
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut log = EventLog::new(V2);
    append(
        op(OperationClass::ScopeOpen, 1, 0),
        &mut sequencer,
        &mut log,
    );
    append(user("head", 1), &mut sequencer, &mut log);
    append(second, &mut sequencer, &mut log);
    append(
        op(OperationClass::Place, 0, Region::Head.rank()),
        &mut sequencer,
        &mut log,
    );
    append(
        op(OperationClass::Place, 1, Region::Body.rank()),
        &mut sequencer,
        &mut log,
    );
    let state = Reducer::new(IDLENESS_WINDOW).fold(&log).unwrap();
    (state, ItemId::append(0), ItemId::append(1))
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
fn legality_table_covers_all_predicates() {
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
            "profile-over-budget",
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
