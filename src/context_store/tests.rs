//! Red-first tests for the Phase 2 external store (issue #39).
use crate::context_store::ops::StoreOperation;
use crate::context_store::spine::{Spine, SpineError};
use crate::context_store::store::{ContextStore, StoreBlocked, StoreError, StoreMode};
use crate::context_store::vault::{Vault, VaultError, VaultKey, VaultSlotSnapshot, VaultSnapshot};

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

/// `recovered_tail_records` reports an honest count of dropped tail frames,
/// not a boolean: a corrupt frame drops the frames behind it too, because a
/// salvage cannot resume past a frame it could not verify, so a truncated run
/// that still contains several well-framed frames reports every frame it
/// recovered away (final-review finding). The old accessor collapsed any
/// number of dropped frames into a single `usize::from(...)`, so crash-salvage
/// reporting could never say how much was lost.
#[test]
fn recovered_tail_records_counts_every_dropped_frame() {
    let mut spine = Spine::new();
    spine.append("a", b"first");
    spine.append("b", b"second");
    spine.append("c", b"third");
    spine.append("d", b"fourth");
    let mut encoded = spine.encode();

    // Corrupt the SECOND frame's digest: that frame fails its validation and
    // the two frames after it are dropped behind the first unverifiable one.
    // The second record's body range ends where its digest begins, and the
    // digest occupies the 8 bytes that follow it, so the frame's last byte is
    // the digest's final byte.
    // Records carry only their body ranges, so walk the encoded frames to find
    // where the second frame's digest ends: each frame is
    // 4 bytes of length + body + 8 bytes of digest.
    let first_len = (spine.records()[0].range.end - spine.records()[0].range.start) as usize;
    let second_len = (spine.records()[1].range.end - spine.records()[1].range.start) as usize;
    let second_digest_last = 4 + first_len + 8 + 4 + second_len + 8 - 1;
    assert!(second_digest_last < encoded.len());
    encoded[second_digest_last] ^= 0xff;
    let recovered = Spine::load(&encoded);
    assert_eq!(
        recovered.records().len(),
        1,
        "only the first frame survives"
    );
    assert_eq!(
        recovered.recovered_tail_records(),
        3,
        "every dropped tail frame is counted, not collapsed into a yes/no"
    );

    // The intact spine still reports zero, so the count is not a disguised flag.
    let mut intact = Spine::new();
    intact.append("a", b"one");
    assert_eq!(Spine::load(&intact.encode()).recovered_tail_records(), 0);

    // A trailing partial frame counts as one dropped frame too.
    let mut truncated = intact.encode();
    truncated.truncate(truncated.len() - 2);
    let salvaged = Spine::load(&truncated);
    assert_eq!(salvaged.records().len(), 0);
    assert_eq!(salvaged.recovered_tail_records(), 1);
}

/// `restore` refuses a live slot whose recorded nonce is not the length the
/// AEAD in use requires: accepting it would leave a slot whose first `get`
/// panics inside `Nonce::from_slice`. The refusal is typed and happens at
/// restore time, before any state is adopted (final-review finding).
#[test]
fn vault_restore_refuses_a_nonce_of_the_wrong_length() {
    let mut vault = Vault::open_with_prefix(&key(), 1);
    let handle = vault.put(b"secret-material", "detector-failed").unwrap();
    let mut snapshot = vault.snapshot();
    // An 8-byte nonce is not a GCM nonce, so the snapshot cannot be accepted.
    let live = snapshot
        .slots
        .iter_mut()
        .find(|slot| slot.handle == handle)
        .expect("the sealed handle is in the snapshot");
    live.nonce = "0011223344556677".to_string();
    let mut restored = Vault::open_with_prefix(&key(), 2);
    assert_eq!(
        restored.restore(snapshot).unwrap_err(),
        VaultError::NonceLength,
        "a wrong-length nonce must be refused at restore, not on first use"
    );
    assert!(
        restored.live_slots() == 0,
        "a refused restore adopts nothing"
    );

    // The empty nonce a tombstone records is fine: tombstones hold no nonce.
    let mut tombstoned = Vault::open_with_prefix(&key(), 3);
    let erased = tombstoned.put(b"gone", "detector-failed").unwrap();
    tombstoned.erase(&erased).unwrap();
    let mut accepted = Vault::open_with_prefix(&key(), 4);
    accepted.restore(tombstoned.snapshot()).unwrap();
    assert!(accepted.is_erased(&erased));
}

/// `next` can never mint a `vault-<reason>-<slot>` number the snapshot's own
/// slots already use: a snapshot that contradicts its own slots is refused, and
/// one whose recorded counter clears its slots restores with the counter
/// floored past every embedded slot number, so the first seal after a restart
/// cannot reuse a handle (and therefore a nonce).
#[test]
fn vault_restore_floors_the_slot_counter_past_its_own_slots() {
    let mut vault = Vault::open_with_prefix(&key(), 5);
    vault.put(b"first", "detector-failed").unwrap();
    vault.put(b"second", "detector-timeout").unwrap();
    vault.erase("vault-detector-failed-0").unwrap();
    let snapshot = vault.snapshot();
    assert_eq!(snapshot.next, 2);

    // A counter recorded as if only one seal ever happened contradicts the two
    // slots the snapshot itself carries, so it is refused outright.
    let mut inconsistent = snapshot.clone();
    inconsistent.next = 1;
    let mut restored = Vault::open_with_prefix(&key(), 6);
    assert_eq!(
        restored.restore(inconsistent).unwrap_err(),
        VaultError::Unavailable,
        "a snapshot whose next contradicts its own slots is refused"
    );
    assert!(
        restored.live_slots() == 0,
        "a refused restore adopts nothing"
    );

    // A consistent snapshot restores and every later seal mints a FRESH slot
    // number: no handle the snapshot already holds can be minted again.
    let mut accepted = Vault::open_with_prefix(&key(), 7);
    accepted.restore(snapshot).unwrap();
    assert_eq!(
        accepted.live_slots(),
        1,
        "the erased slot stays a tombstone"
    );
    for _ in 0..4 {
        let minted = accepted.put(b"after restart", "detector-failed").unwrap();
        assert_ne!(
            minted, "vault-detector-failed-0",
            "no restored slot number is ever re-minted"
        );
        assert_ne!(minted, "vault-detector-timeout-1");
    }
    assert_eq!(accepted.live_slots(), 5);
}

/// A live handle the vault itself never minted cannot be decrypted through this
/// port, so it is refused at restore time instead of being adopted as a slot
/// whose read can only fail later.
#[test]
fn vault_restore_refuses_a_foreign_live_handle() {
    let foreign = VaultSnapshot {
        nonce_prefix: 0,
        next: 1,
        slots: vec![VaultSlotSnapshot {
            handle: "not-a-vault-handle".to_string(),
            // 12 zero bytes, hex-encoded: a valid-length nonce for the AEAD.
            nonce: "000000000000000000000000".to_string(),
            ciphertext: "00".to_string(),
            erased: false,
        }],
    };
    let mut refused = Vault::open_with_prefix(&key(), 10);
    assert_eq!(
        refused.restore(foreign).unwrap_err(),
        VaultError::Unavailable
    );
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
    store.sanitized_append(Some("h0"), b"bytes").unwrap();
    for mode in [StoreMode::ReadOnly, StoreMode::Unavailable] {
        store.set_mode(mode);
        assert_eq!(
            store.begin_state_advancing_turn().unwrap_err(),
            StoreBlocked::Mode { mode: mode.name() }
        );
        assert!(store.sanitized_append(Some("h1"), b"more").is_err());
        assert!(store.vault_put(b"raw", "why").is_err());
        // Reads still work: only state advancement and side effects are blocked.
        assert_eq!(store.read_page(0..5, 16).unwrap().bytes, b"bytes");
    }
    store.set_mode(StoreMode::Normal);
    store.sanitized_append(Some("h2"), b"again").unwrap();
}

#[test]
fn store_index_rebuild_and_range_selector() {
    let mut store = ContextStore::open(&key());
    store.sanitized_append(Some("h0"), b"aaaa").unwrap();
    store.sanitized_append(Some("h1"), b"bbbb").unwrap();
    store.sanitized_append(Some("h2"), b"cccc").unwrap();
    assert_eq!(store.rebuild_index(), 3);
    let hits = store.select(4..8);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].handle, "h1");
    assert_eq!(store.select(100..200).len(), 0);
}

#[test]
fn store_checkpoint_tail_replay_is_exact() {
    let mut store = ContextStore::open(&key());
    store.sanitized_append(Some("h0"), b"one").unwrap();
    let checkpoint = store.checkpoint();
    store.sanitized_append(Some("h1"), b"two").unwrap();
    store.sanitized_append(Some("h2"), b"three").unwrap();
    let tail = store.replay_tail(checkpoint);
    let handles: Vec<&str> = tail.iter().map(|record| record.handle.as_str()).collect();
    assert_eq!(handles, ["h1", "h2"]);
    assert_eq!(store.latest_checkpoint(), Some(checkpoint));
}

#[test]
fn store_loads_encoded_spine_and_rebuilds_the_index() {
    let mut store = ContextStore::open(&key());
    store.sanitized_append(Some("h0"), b"one").unwrap();
    store.sanitized_append(Some("h1"), b"two").unwrap();
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
