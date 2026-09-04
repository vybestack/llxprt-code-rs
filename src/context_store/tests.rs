//! Red-first tests for the Phase 2 external store (issue #39).
use crate::context_store::ops::StoreOperation;
use crate::context_store::spine::{Spine, SpineError};
use crate::context_store::store::{ContextStore, StoreBlocked, StoreError, StoreMode};
use crate::context_store::vault::{Vault, VaultError, VaultKey};

fn key() -> VaultKey {
    let mut key = [0u8; 32];
    for (index, byte) in key.iter_mut().enumerate() {
        *byte = index as u8;
    }
    VaultKey::from(key)
}

fn payload() -> Vec<u8> {
    let mut bytes = Vec::new();
    for round in 0..64u32 {
        bytes.extend_from_slice(format!("round {round}: sanitized line\n").as_bytes());
    }
    bytes
}

#[test]
fn spine_reads_are_byte_preserving_and_bounded() {
    let mut spine = Spine::new();
    let bytes = payload();
    let range = spine.append("h0", &bytes);
    assert_eq!(range, 0..bytes.len() as u64);
    assert_eq!(spine.read(0..16, 32).unwrap(), bytes[..16].to_vec());
    let err = spine.read(0..256, 32).unwrap_err();
    assert!(matches!(err, SpineError::RangeOutside { .. }));
    let err = spine
        .read(0..(bytes.len() as u64 + 1), 1 << 20)
        .unwrap_err();
    assert!(matches!(err, SpineError::RangeOutside { .. }));
}

#[test]
fn spine_pagination_covers_a_range_in_order() {
    let mut spine = Spine::new();
    let bytes = payload();
    let full = spine.append("h0", &bytes);
    let mut cursor = full.clone();
    let mut rebuilt = Vec::new();
    while let Some(page) = Some(spine.read_page(cursor.clone(), 64).unwrap()) {
        rebuilt.extend_from_slice(&page.bytes);
        match page.remaining {
            Some(next) => cursor = next,
            None => break,
        }
    }
    assert_eq!(rebuilt, bytes);
}

#[test]
fn spine_framing_round_trips_and_recovers_corrupt_tails() {
    let mut spine = Spine::new();
    spine.append("a", b"first");
    spine.append("b", b"second");
    let mut encoded = spine.encode();
    let good = Spine::load(&encoded);
    assert_eq!(good.records().len(), 2);
    assert_eq!(good.recovered_tail_records(), 0);
    let truncated = &encoded[..encoded.len() - 3];
    let recovered = Spine::load(truncated);
    assert_eq!(recovered.records().len(), 1, "tail frame must be dropped");
    assert_eq!(recovered.recovered_tail_records(), 1);
    // Corrupt the digest of the last frame.
    let last = encoded.len() - 1;
    encoded[last] ^= 0xff;
    let recovered = Spine::load(&encoded);
    assert_eq!(recovered.records().len(), 1);
    assert_eq!(recovered.recovered_tail_records(), 1);
}

#[test]
fn vault_seals_and_erases_with_tombstones() {
    let mut vault = Vault::open(&key());
    let handle = vault
        .put(b"CTXEVAL-SECRET-A1B2C3D4E5", "detector-failed")
        .unwrap();
    assert_eq!(vault.get(&handle).unwrap(), b"CTXEVAL-SECRET-A1B2C3D4E5");
    assert_eq!(vault.live_slots(), 1);
    vault.erase(&handle).unwrap();
    assert!(vault.is_erased(&handle));
    assert_eq!(vault.get(&handle).unwrap_err(), VaultError::UnknownHandle);
    assert_eq!(vault.live_slots(), 0);
    assert_eq!(
        vault.erase("vault-missing").unwrap_err(),
        VaultError::UnknownHandle
    );
}

#[test]
fn store_modes_block_state_advancing_turns() {
    let mut store = ContextStore::open(&key());
    assert_eq!(store.mode(), StoreMode::Normal);
    store.sanitized_append("h0", b"bytes").unwrap();
    for mode in [StoreMode::ReadOnly, StoreMode::Unavailable] {
        store.set_mode(mode);
        assert_eq!(
            store.begin_state_advancing_turn().unwrap_err(),
            StoreBlocked::Mode { mode: mode.name() }
        );
        assert!(store.sanitized_append("h1", b"more").is_err());
        assert!(store.vault_put(b"raw", "why").is_err());
        // Reads still work: only state advancement and side effects are blocked.
        assert_eq!(store.read_page(0..5, 16).unwrap().bytes, b"bytes");
    }
    store.set_mode(StoreMode::Normal);
    store.sanitized_append("h2", b"again").unwrap();
}

#[test]
fn store_index_rebuild_and_range_selector() {
    let mut store = ContextStore::open(&key());
    store.sanitized_append("h0", b"aaaa").unwrap();
    store.sanitized_append("h1", b"bbbb").unwrap();
    store.sanitized_append("h2", b"cccc").unwrap();
    assert_eq!(store.rebuild_index(), 3);
    let hits = store.select(4..8);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].handle, "h1");
    assert_eq!(store.select(100..200).len(), 0);
}

#[test]
fn store_checkpoint_tail_replay_is_exact() {
    let mut store = ContextStore::open(&key());
    store.sanitized_append("h0", b"one").unwrap();
    let checkpoint = store.checkpoint();
    store.sanitized_append("h1", b"two").unwrap();
    store.sanitized_append("h2", b"three").unwrap();
    let tail = store.replay_tail(checkpoint);
    let handles: Vec<&str> = tail.iter().map(|record| record.handle.as_str()).collect();
    assert_eq!(handles, ["sanitized-1", "sanitized-2"]);
    assert_eq!(store.latest_checkpoint(), Some(checkpoint));
}

#[test]
fn store_loads_encoded_spine_and_rebuilds_the_index() {
    let mut store = ContextStore::open(&key());
    store.sanitized_append("h0", b"one").unwrap();
    store.sanitized_append("h1", b"two").unwrap();
    let encoded = store.spine_bytes();
    let mut fresh = ContextStore::open(&key());
    assert_eq!(fresh.load_spine(&encoded), 0);
    assert_eq!(fresh.recovered_tail_records(), 0);
    assert_eq!(fresh.rebuild_index(), 2);
    assert_eq!(fresh.read_page(0..3, 8).unwrap().bytes, b"one");
    assert_eq!(
        fresh.vault_get("vault-nope").unwrap_err(),
        StoreError::Vault(VaultError::UnknownHandle)
    );
}

#[test]
fn phase2_operation_rows_are_named_and_complete() {
    let names: Vec<&str> = StoreOperation::all().iter().map(|op| op.name()).collect();
    for expected in [
        "admit-ingress",
        "sanitize",
        "redact",
        "import",
        "rule-update",
        "vocabulary-update",
        "index-rebuild",
        "store-mode",
        "quiesce-unwritable",
    ] {
        assert!(
            names.contains(&expected),
            "missing operation row {expected}"
        );
    }
    assert!(!StoreOperation::QuiesceUnwritable.advances_state());
    assert!(StoreOperation::AdmitIngress.advances_state());
}

#[test]
fn phase2_rows_carry_the_registry_names_in_order() {
    use crate::context_kernel::events::OperationClass;

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

// ===========================================================================
// Migration flow tests (moved from src/context_kernel/tests/migration.rs when the
// context_kernel -> context_store edge was removed; these exercise the store + kernel
// migration machinery together, so they live on the store side where the kernel is the
// dependency).
// ===========================================================================

use crate::context_kernel::events::{
    AppendSource, EventKind, EventLog, OperationClass, RecordedEvent, Sequencer, FIRST_SEQUENCE,
};
use crate::context_kernel::ir::StoreRange;
use crate::context_kernel::migration::{
    decide, Generation, MigrationDescriptor, MigrationPlan, PrivateBuild, Publication, SlotPair,
    V2, V3,
};
use crate::context_kernel::reducer::{Reducer, IDLENESS_WINDOW};

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

/// The v2 side of the migration flow: a live store with two records, and the plan
/// that names the spine ranges the private build copies. Returns the store's spine
/// bytes, the log the migration is recorded in, and the sequencer that continues it.
fn v2_store_for_the_flow() -> (Vec<u8>, EventLog, Sequencer) {
    let mut raw = [0u8; 32];
    for (index, byte) in raw.iter_mut().enumerate() {
        *byte = index as u8;
    }
    let key = VaultKey::from(raw);
    let mut v2_store = ContextStore::open(&key);
    v2_store
        .sanitized_append("v2-record-a", &[1_u8; 16])
        .unwrap();
    v2_store
        .sanitized_append("v2-record-b", &[2_u8; 16])
        .unwrap();
    let v2_bytes = v2_store.spine_bytes();

    let mut sequencer = Sequencer::new(FIRST_SEQUENCE, 1, 1_000);
    let mut log = EventLog::new(V2);
    append(
        op(OperationClass::ScopeOpen, 1, 0),
        &mut sequencer,
        &mut log,
    );
    append(user("before", 1), &mut sequencer, &mut log);
    assert_eq!(decide(&log).store_version(), V2, "nothing selected yet");
    (v2_bytes, log, sequencer)
}

/// The completed private build that copied the plan's ranges out of the v2 spine:
/// the build itself, its copied bytes, and the publication it frames.
fn built_publication_for_the_flow(
    v2_bytes: &[u8],
    log: &EventLog,
) -> (PrivateBuild, Vec<u8>, Publication) {
    let plan = MigrationPlan::from(
        V3,
        vec![
            StoreRange {
                offset: 0,
                length: 16,
            },
            StoreRange {
                offset: 16,
                length: 16,
            },
        ],
        log.head_checksum(),
    );
    assert_eq!(plan.units(), 32);
    let mut build = PrivateBuild::start(plan);
    assert!(
        Publication::of(&build).is_none(),
        "an incomplete build never publishes"
    );

    // Copy the plan's bytes into the private build.
    let mut copied: Vec<u8> = Vec::new();
    for range in &build.plan.ranges {
        copied.extend_from_slice(&v2_bytes[range.offset as usize..range.end() as usize]);
    }
    build.complete_with(&copied);
    let publication = Publication::of(&build).unwrap();
    (build, copied, publication)
}

/// GREEN: the private build copies the plan's ranges out of the live v2 store, and
/// the publication it frames verifies only inside the store-build hash scope.
#[test]
fn the_private_build_copies_the_plan_in_the_store_build_scope() {
    let (v2_bytes, log, _sequencer) = v2_store_for_the_flow();
    let (_build, copied, publication) = built_publication_for_the_flow(&v2_bytes, &log);
    assert_eq!(publication.store_version, V3);
    assert!(
        publication.verify_build(&copied),
        "the build checksum is scoped"
    );
    assert!(!publication.verify_build(&v2_bytes), "the scopes never mix");
}

/// GREEN: the build lands in the inactive slot while the readers still resolve v2,
/// the recorded selection switches the crash-matrix decision, and the swap makes
/// v3 the visible generation — verified against both hash scopes.
#[test]
fn the_flow_lands_swaps_and_verifies_in_its_scopes() {
    let (v2_bytes, mut log, mut sequencer) = v2_store_for_the_flow();
    let (_build, copied, publication) = built_publication_for_the_flow(&v2_bytes, &log);

    let mut slots = SlotPair::genesis(V2, v2_bytes.len() as u64, log.head_checksum());
    slots
        .land(Generation::Built {
            store_version: V3,
            bytes: copied.len() as u64,
            checksum: publication.built_checksum,
        })
        .unwrap();
    assert_eq!(slots.active().store_version(), V2, "landing is invisible");

    // Record the selection, then swap.
    append(
        op(OperationClass::MigrationSelect, V3, 0),
        &mut sequencer,
        &mut log,
    );
    assert_eq!(
        decide(&log).store_version(),
        V3,
        "the recorded selection switches the crash-matrix decision"
    );
    let descriptor = MigrationDescriptor::seal(V3, publication.built_checksum, log.head_checksum());
    assert_eq!(
        slots
            .swap(descriptor.selection_chain)
            .unwrap()
            .store_version(),
        V3,
        "the swap is the visibility switch"
    );
    assert!(descriptor.verify_build(&copied));
    assert!(descriptor.verify_chain(&log));

    // And the log still replays to identical state, framing version intact.
    let state = Reducer::new(IDLENESS_WINDOW).fold(&log).unwrap();
    assert_eq!(state.store_version, V2);
    assert_eq!(state.selected_store_version, Some(V3));
}
