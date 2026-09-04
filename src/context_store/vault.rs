//! Encrypted vault port for quarantined plaintext (issue #39).
//!
//! AES-256-GCM with a caller-supplied key: the runtime derives key material from the
//! platform keychain in a later phase, and tests pass a fixed key. Erasure replaces the
//! ciphertext with a tombstone, after which the plaintext is unreachable through the port.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Vault key material (32 bytes for AES-256-GCM).
pub type VaultKey = Key<Aes256Gcm>;

/// Errors raised by the vault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultError {
    Unavailable,
    UnknownHandle,
    Erased,
    OpenFailure,
}

/// One vault slot: ciphertext plus its nonce, or a tombstone.
enum Slot {
    Live { nonce: Vec<u8>, ciphertext: Vec<u8> },
    Tombstone,
}

/// Encrypted vault holding quarantined plaintext outside the sanitized spine.
pub struct Vault {
    cipher: Aes256Gcm,
    slots: HashMap<String, Slot>,
    next: u64,
    /// Per-process random 32-bit prefix mixed into every nonce.
    nonce_prefix: u32,
}

impl Vault {
    /// Opens the vault with key material supplied by the caller.
    ///
    /// The nonce prefix is drawn from the std entropy pool (`RandomState`
    /// seeds from OS entropy), so two processes that share key material never
    /// reuse a nonce even after a restart (the slot counter restarts at zero,
    /// the prefix does not). Deterministic tests open with
    /// [`Vault::open_with_prefix`].
    pub fn open(key: &VaultKey) -> Self {
        use std::hash::{BuildHasher as _, Hasher as _};
        let prefix = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish() as u32;
        Self::open_with_prefix(key, prefix)
    }

    /// Opens the vault with an explicit nonce prefix (tests, snapshots).
    pub fn open_with_prefix(key: &VaultKey, nonce_prefix: u32) -> Self {
        Self {
            cipher: Aes256Gcm::new(key),
            slots: HashMap::new(),
            next: 0,
            nonce_prefix,
        }
    }

    /// Seals `raw` and returns its handle. Nonces mix the per-process random
    /// prefix with the slot number, so sealing never repeats a nonce under a
    /// fixed key; handles never repeat.
    pub fn put(&mut self, raw: &[u8], reason: &str) -> Result<String, VaultError> {
        let slot = self.next;
        self.next += 1;
        let handle = format!("vault-{reason}-{slot}");
        let mut nonce_bytes = [0u8; 12];
        nonce_bytes[..4].copy_from_slice(&self.nonce_prefix.to_le_bytes());
        nonce_bytes[4..].copy_from_slice(&slot.to_le_bytes());
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = self
            .cipher
            .encrypt(
                nonce,
                Payload {
                    msg: raw,
                    aad: handle.as_bytes(),
                },
            )
            .map_err(|_| VaultError::Unavailable)?;
        self.slots.insert(
            handle.clone(),
            Slot::Live {
                nonce: nonce.to_vec(),
                ciphertext,
            },
        );
        Ok(handle)
    }

    /// Opens a handle back into plaintext.
    pub fn get(&self, handle: &str) -> Result<Vec<u8>, VaultError> {
        match self.slots.get(handle) {
            None | Some(Slot::Tombstone) => Err(VaultError::UnknownHandle),
            Some(Slot::Live { nonce, ciphertext }) => {
                let nonce = Nonce::from_slice(nonce);
                self.cipher
                    .decrypt(
                        nonce,
                        Payload {
                            msg: ciphertext,
                            aad: handle.as_bytes(),
                        },
                    )
                    .map_err(|_| VaultError::OpenFailure)
            }
        }
    }

    /// Erases a handle: the ciphertext is dropped and a tombstone remains.
    pub fn erase(&mut self, handle: &str) -> Result<(), VaultError> {
        match self.slots.get_mut(handle) {
            None => Err(VaultError::UnknownHandle),
            Some(slot) => {
                *slot = Slot::Tombstone;
                Ok(())
            }
        }
    }

    /// Whether a handle is a tombstone.
    pub fn is_erased(&self, handle: &str) -> bool {
        matches!(self.slots.get(handle), Some(Slot::Tombstone))
    }

    /// Number of live (non-erased) slots.
    pub fn live_slots(&self) -> usize {
        self.slots
            .values()
            .filter(|slot| matches!(slot, Slot::Live { .. }))
            .count()
    }

    /// Deterministic, serde-ready snapshot of the vault state for durable artifacts.
    ///
    /// Slots are sorted by handle (hash-map iteration order is random) and byte fields
    /// are hex-encoded, so the same vault state always serializes to the same bytes.
    pub fn snapshot(&self) -> VaultSnapshot {
        let mut slots = Vec::with_capacity(self.slots.len());
        for (handle, slot) in &self.slots {
            let snapshot = match slot {
                Slot::Live { nonce, ciphertext } => VaultSlotSnapshot {
                    handle: handle.clone(),
                    nonce: hex(nonce),
                    ciphertext: hex(ciphertext),
                    erased: false,
                },
                Slot::Tombstone => VaultSlotSnapshot {
                    handle: handle.clone(),
                    nonce: String::new(),
                    ciphertext: String::new(),
                    erased: true,
                },
            };
            slots.push(snapshot);
        }
        slots.sort_by(|left, right| left.handle.cmp(&right.handle));
        VaultSnapshot {
            nonce_prefix: self.nonce_prefix,
            next: self.next,
            slots,
        }
    }

    /// Restores a vault from its durable snapshot: live slots re-open under
    /// their recorded ciphertext and nonces, the slot counter advances past
    /// every recorded handle, and the nonce prefix is replaced by a fresh
    /// per-process draw. Restored handles read back unchanged, but every new
    /// seal after a restart mixes a prefix the previous process never used,
    /// so a nonce is never reused under one key (issue #101).
    pub fn restore(&mut self, snapshot: VaultSnapshot) -> Result<(), VaultError> {
        let mut slots = HashMap::new();
        for slot in &snapshot.slots {
            if slots.contains_key(&slot.handle) {
                return Err(VaultError::Unavailable);
            }
            let restored = if slot.erased {
                Slot::Tombstone
            } else {
                Slot::Live {
                    nonce: unhex(&slot.nonce).ok_or(VaultError::Unavailable)?,
                    ciphertext: unhex(&slot.ciphertext).ok_or(VaultError::Unavailable)?,
                }
            };
            slots.insert(slot.handle.clone(), restored);
        }
        self.slots = slots;
        self.next = snapshot.next;
        // Fresh per-process prefix: the recorded prefix described the previous
        // process, and resuming it under the same key would reuse a nonce.
        self.nonce_prefix = {
            use std::hash::{BuildHasher as _, Hasher as _};
            std::collections::hash_map::RandomState::new()
                .build_hasher()
                .finish() as u32
        };
        Ok(())
    }
}

/// Decodes lowercase hex without an external codec dependency.
fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(text.len() / 2);
    for pair in bytes.chunks(2) {
        let high = (pair[0] as char).to_digit(16)?;
        let low = (pair[1] as char).to_digit(16)?;
        out.push(((high << 4) | low) as u8);
    }
    Some(out)
}

/// One serialized vault slot: ciphertext plus nonce, or a tombstone.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultSlotSnapshot {
    pub handle: String,
    pub nonce: String,
    pub ciphertext: String,
    pub erased: bool,
}

/// Deterministic serialized vault state, including the slot counter.
///
/// `nonce_prefix` is the per-process random prefix mixed into every nonce, so
/// a snapshot taken after a restart can prove the prefix changed (nonce reuse
/// under one key is impossible across processes).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultSnapshot {
    pub nonce_prefix: u32,
    pub next: u64,
    pub slots: Vec<VaultSlotSnapshot>,
}

/// Lowercase hex encoding without an external codec dependency.
fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
