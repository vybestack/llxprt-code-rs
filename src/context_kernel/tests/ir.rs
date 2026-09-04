//! Conversation IR tests: atomic claims, lanes, regions, identifiers, splits.

use crate::context_kernel::events::{
    AppendSource, EventKind, EventLog, OperationClass, RecordedEvent, Sequencer, FIRST_SEQUENCE,
};
use crate::context_kernel::ir::{
    covered_units, slice_into, ConversationIr, IrError, Item, ItemId, Region, SegmentClaim,
    SplitContract, SplitNamespace, StoreRange,
};
use crate::context_kernel::lanes::Lane;
use crate::context_kernel::migration::V2;
use crate::context_kernel::reducer::{Reducer, ReducerError, TypedState, IDLENESS_WINDOW};

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

fn placed_state() -> TypedState {
    placed_log_items(user("body", 1)).0
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

/// GREEN: an item enters a region only through a logged Place event. The append
/// constructor places nothing, so a bare append leaves every region empty and
/// occupancy unchanged: a constructor that placed an item would invent a
/// placement no event recorded and silently spend a region's budget.
#[test]
fn a_bare_append_places_nothing_and_a_place_event_places_it() {
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut log = EventLog::new(V2);
    append(
        op(OperationClass::ScopeOpen, 1, 0),
        &mut sequencer,
        &mut log,
    );
    append(user("a plain task statement", 1), &mut sequencer, &mut log);
    let state = Reducer::new(IDLENESS_WINDOW).fold(&log).unwrap();
    for region in Region::all() {
        assert!(
            state.conversation_ir.region_items(region).is_empty(),
            "no region holds an item an append placed by construction"
        );
        assert_eq!(
            state.conversation_ir.region_occupancy(region),
            0,
            "no region is charged for an unplaced append"
        );
    }
    assert!(state.conversation_ir.placed_ids().is_empty());
    assert!(
        state.conversation_ir.items()[0].is_store_only(),
        "a fresh append keeps its bytes out of every region"
    );
    assert_eq!(state.conversation_ir.items()[0].units(), 22);

    // A logged Place is the only way in, and it is a recorded event.
    let mut continuing_log = log.clone();
    let mut resumed = Sequencer::continuing(&log, 1, 2_000);
    append(
        op(OperationClass::Place, 0, Region::Head.rank()),
        &mut resumed,
        &mut continuing_log,
    );
    let moved = Reducer::new(IDLENESS_WINDOW).fold(&continuing_log).unwrap();
    assert_eq!(
        moved.conversation_ir.region_items(Region::Head),
        vec![ItemId::append(0)],
        "the logged Place event moves the item into the head region"
    );
    assert_eq!(
        moved.conversation_ir.region_occupancy(Region::Head),
        22,
        "the region is charged exactly once, for the recorded event"
    );
    assert_eq!(moved.conversation_ir.region_items(Region::Body).len(), 0);
    assert_eq!(moved.applied_len(), 3, "the placement is a recorded event");
}

/// One parent item split into two contiguous halves of `length` each: the shape both
/// split-inheritance tests probe. `region` is the placement the parent carries before the
/// split, so `None` builds the store-only parent.
fn split_parent_in(
    region: Option<Region>,
    lane: Lane,
    length: u64,
) -> (ConversationIr, Vec<ItemId>) {
    let mut ir = ConversationIr::new();
    let id = ir.reserve_append_id();
    ir.insert(Item::new(
        id,
        lane,
        vec![StoreRange {
            offset: 0,
            length: length * 2,
        }],
        1,
    ))
    .unwrap();
    // The parent enters the region through the placement transition, if it has one.
    if let Some(region) = region {
        ir.place(id, region).unwrap();
    }
    let children = ir
        .split(
            id,
            vec![
                vec![StoreRange { offset: 0, length }],
                vec![StoreRange {
                    offset: length,
                    length,
                }],
            ],
            &SplitContract {
                namespace: SplitNamespace::Fresh,
                parts: 2,
                split_points: vec![1, 1],
            },
        )
        .unwrap();
    (ir, children)
}

/// GREEN: a placed parent's children inherit its placement in their own item
/// fields, so the head partition holds exactly the children and occupancy is
/// charged once, to the region the parent sat in.
#[test]
fn split_children_keep_the_parents_head_region() {
    let (ir, children) = split_parent_in(Some(Region::Head), Lane::Constitutional, 4);
    assert_eq!(children.len(), 2);
    for child in &children {
        assert_eq!(
            ir.item(*child).unwrap().region(),
            Some(Region::Head),
            "each child carries the parent's placement in its own field"
        );
    }
    let mut head = ir.region_items(Region::Head);
    head.sort();
    let mut expected = children.clone();
    expected.sort();
    assert_eq!(head, expected, "the head partition holds the children");
    assert!(
        ir.region_items(Region::Body).is_empty(),
        "the split moved nothing into another region"
    );
    assert!(
        ir.region_items(Region::Notes).is_empty(),
        "the split moved nothing into the notes region"
    );
    assert!(
        ir.region_items(Region::Tail).is_empty(),
        "the split moved nothing into the tail region"
    );
    assert_eq!(
        ir.region_occupancy(Region::Head),
        8,
        "occupancy is charged once, to the region the parent sat in"
    );
}

/// GREEN: a store-only parent keeps its children store-only: no placement is
/// invented, so no child lands in a region and nothing is charged to one.
#[test]
fn store_only_children_inherit_no_placement() {
    let (other, inherited) = split_parent_in(None, Lane::Evidential, 3);
    for child in &inherited {
        assert!(
            other.item(*child).unwrap().is_store_only(),
            "a store-only parent leaves its children store-only"
        );
    }
    assert_eq!(other.placed_ids().len(), 0);
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
