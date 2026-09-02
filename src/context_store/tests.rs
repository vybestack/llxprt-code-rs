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
