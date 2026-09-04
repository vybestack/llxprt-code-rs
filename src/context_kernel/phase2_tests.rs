//! Phase 2 operation rows: names, transitions, typed refusals, replay invariants.

use crate::context_kernel::events::{
    AppendSource, EventKind, EventLog, OperationClass, Sequencer, FIRST_SEQUENCE,
};
use crate::context_kernel::ir::{ItemId, Region};
use crate::context_kernel::reducer::{
    Reducer, ReducerError, IDLENESS_WINDOW, INITIAL_VERSION, STORE_MODE_UNAVAILABLE,
};
use crate::context_store::ops::StoreOperation;

/// Appends an event, returning its sequence. Append identifiers are minted from
/// the IR's counter, so tests that need to name a folded item count the log's
/// append events instead of reusing the sequence.
fn append(kind: EventKind, sequencer: &mut Sequencer, log: &mut EventLog) -> u64 {
    let event = sequencer.append(kind, log.store_version());
    log.append(event.clone()).unwrap();
    event.sequence
}

/// Identifier the fold mints for the log's `index`-th append event, zero-based.
fn append_item_id(log: &EventLog, index: usize) -> ItemId {
    let appends = log
        .events()
        .iter()
        .filter(|event| matches!(event.kind, EventKind::Append { .. }))
        .count();
    assert!(
        index < appends,
        "the log holds fewer appends than the index"
    );
    ItemId::new(index as u64)
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

fn single(kind: EventKind) -> EventLog {
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut log = EventLog::new(1);
    append(kind, &mut sequencer, &mut log);
    log
}

fn fold_err(log: &EventLog) -> ReducerError {
    Reducer::new(IDLENESS_WINDOW).fold(log).unwrap_err()
}

/// Maps the Phase 2 registry row onto its kernel class.
fn kernel_row(row: StoreOperation) -> OperationClass {
    match row {
        StoreOperation::AdmitIngress => OperationClass::AdmitIngress,
        StoreOperation::Sanitize => OperationClass::Sanitize,
        StoreOperation::Redact => OperationClass::Redact,
        StoreOperation::Import => OperationClass::Import,
        StoreOperation::RuleUpdate => OperationClass::RuleUpdate,
        StoreOperation::VocabularyUpdate => OperationClass::VocabularyUpdate,
        StoreOperation::IndexRebuild => OperationClass::IndexRebuild,
        StoreOperation::StoreMode => OperationClass::StoreMode,
        StoreOperation::QuiesceUnwritable => OperationClass::QuiesceUnwritable,
    }
}

#[test]
fn phase2_rows_carry_the_registry_names_in_order() {
    let rows = StoreOperation::all();
    let named: Vec<&'static str> = rows.iter().map(|row| kernel_row(*row).name()).collect();
    assert_eq!(
        named,
        vec![
            "admit-ingress",
            "sanitize",
            "redact",
            "import",
            "rule-update",
            "vocabulary-update",
            "index-rebuild",
            "store-mode",
            "quiesce-unwritable",
        ]
    );
}

#[test]
fn admit_ingress_advances_the_spine_cursor_and_attributes_the_scope() {
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut log = EventLog::new(1);
    append(
        op(OperationClass::ScopeOpen, 7, 0),
        &mut sequencer,
        &mut log,
    );
    append(
        op(OperationClass::AdmitIngress, 7, 10),
        &mut sequencer,
        &mut log,
    );
    append(user("sanitized payload", 7), &mut sequencer, &mut log);
    let state = Reducer::new(IDLENESS_WINDOW).fold(&log).unwrap();
    let admitted = state.conversation_ir.item(append_item_id(&log, 0)).unwrap();
    assert_eq!(
        admitted.provenance[0].offset, 10,
        "the admission advances the spine cursor"
    );
    assert_eq!(state.applied_len(), 3);
}

fn redact_log(redact: bool) -> (EventLog, u64) {
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut log = EventLog::new(1);
    append(
        op(OperationClass::ScopeOpen, 1, 0),
        &mut sequencer,
        &mut log,
    );
    append(user("secret span", 1), &mut sequencer, &mut log);
    let item = append_item_id(&log, 0).value();
    append(
        op(OperationClass::Place, item, Region::Notes.rank()),
        &mut sequencer,
        &mut log,
    );
    append(op(OperationClass::Pin, item, 0), &mut sequencer, &mut log);
    if redact {
        append(
            op(OperationClass::Redact, item, 0),
            &mut sequencer,
            &mut log,
        );
    }
    (log, item)
}

#[test]
fn redact_unplaces_and_unpins_while_keeping_the_item() {
    let (plain, _) = redact_log(false);
    let (redacted, item) = redact_log(true);
    let without = Reducer::new(IDLENESS_WINDOW).fold(&plain).unwrap();
    let with = Reducer::new(IDLENESS_WINDOW).fold(&redacted).unwrap();
    assert!(with.pins.is_empty(), "a vaulted item keeps no pin");
    assert!(with.conversation_ir.item(ItemId::new(item)).is_ok());
    assert_eq!(item, 0, "the first append mints the first append id");
    assert_ne!(
        with.state_hash, without.state_hash,
        "redact is a real transition"
    );
}

#[test]
fn registry_rows_are_compare_and_commit() {
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut log = EventLog::new(1);
    append(
        op(OperationClass::RuleUpdate, 1, INITIAL_VERSION),
        &mut sequencer,
        &mut log,
    );
    append(
        op(OperationClass::VocabularyUpdate, 1, 1),
        &mut sequencer,
        &mut log,
    );
    let state = Reducer::new(IDLENESS_WINDOW).fold(&log).unwrap();
    assert_eq!(state.filter_rule_version, 1);
    assert_eq!(state.vocabulary_version, 1);

    let conflict = single(op(OperationClass::RuleUpdate, 1, 9));
    assert_eq!(
        fold_err(&conflict),
        ReducerError::VersionConflict {
            claimed_parent: 9,
            actual: INITIAL_VERSION,
        }
    );
    let unsupported = single(op(OperationClass::VocabularyUpdate, 2, INITIAL_VERSION));
    assert_eq!(
        fold_err(&unsupported),
        ReducerError::UnsupportedVersion {
            requested: 2,
            latest: 1,
        }
    );
}

#[test]
fn store_mode_rows_change_mode_and_refuse_undefined_codes() {
    let log = single(op(OperationClass::StoreMode, 2, 0));
    let state = Reducer::new(IDLENESS_WINDOW).fold(&log).unwrap();
    assert_eq!(state.store_mode, 2);
    let unavailable = single(op(OperationClass::StoreMode, STORE_MODE_UNAVAILABLE, 0));
    assert_eq!(
        Reducer::new(IDLENESS_WINDOW)
            .fold(&unavailable)
            .unwrap()
            .store_mode,
        STORE_MODE_UNAVAILABLE
    );
    for code in [0_u64, STORE_MODE_UNAVAILABLE + 1] {
        let bad = single(op(OperationClass::StoreMode, code, 0));
        assert_eq!(
            fold_err(&bad),
            ReducerError::UnknownStoreMode { found: code }
        );
    }
}

#[test]
fn quiesce_unwritable_records_quiesce_without_advancing_state() {
    let log = single(op(OperationClass::QuiesceUnwritable, 0, 0));
    let state = Reducer::new(IDLENESS_WINDOW).fold(&log).unwrap();
    assert!(state.quiesced);
    assert_eq!(state.version, INITIAL_VERSION);
    assert_eq!(state.applied_len(), 1);
}

#[test]
fn executor_landed_later_rows_are_typed_refusals() {
    let rows = [
        OperationClass::Sanitize,
        OperationClass::Import,
        OperationClass::IndexRebuild,
    ];
    for class in rows {
        let log = single(op(class.clone(), 1, 0));
        assert_eq!(
            fold_err(&log),
            ReducerError::OperationNotLanded {
                operation: class.name(),
            }
        );
    }
}

#[test]
fn phase2_rows_replay_and_dedup_deterministically() {
    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut log = EventLog::new(1);
    append(
        op(OperationClass::ScopeOpen, 1, 0),
        &mut sequencer,
        &mut log,
    );
    append(
        op(OperationClass::AdmitIngress, 1, 10),
        &mut sequencer,
        &mut log,
    );
    append(user("payload", 1), &mut sequencer, &mut log);
    let item = append_item_id(&log, 0).value();
    append(
        op(OperationClass::Place, item, Region::Notes.rank()),
        &mut sequencer,
        &mut log,
    );
    append(
        op(OperationClass::StoreMode, 2, 0),
        &mut sequencer,
        &mut log,
    );
    append(
        op(OperationClass::QuiesceUnwritable, 0, 0),
        &mut sequencer,
        &mut log,
    );
    append(
        op(OperationClass::Redact, item, 0),
        &mut sequencer,
        &mut log,
    );

    let full = Reducer::new(IDLENESS_WINDOW).fold(&log).unwrap();
    let mut partial = Reducer::new(IDLENESS_WINDOW).fold(&log.prefix(3)).unwrap();
    assert_eq!(partial.applied_len(), 3);
    Reducer::new(IDLENESS_WINDOW)
        .fold_from(&mut partial, &log)
        .unwrap();
    assert_eq!(partial.applied_len(), full.applied_len());
    assert_eq!(partial.state_hash, full.state_hash);
    Reducer::new(IDLENESS_WINDOW)
        .fold_from(&mut partial, &log)
        .unwrap();
    assert_eq!(
        partial.applied_len(),
        full.applied_len(),
        "replayed identities are deduplicated"
    );
    assert_eq!(partial.state_hash, full.state_hash);
}
