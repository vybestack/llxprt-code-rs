//! Encrypted vault port for quarantined plaintext (issue #39).
//!
//! AES-256-GCM with a caller-supplied key: the runtime derives key material from the
//! platform keychain in a later phase, and tests pass a fixed key. Erasure replaces the
//! ciphertext with a tombstone, after which the plaintext is unreachable through the port.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
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
}

impl Vault {
    /// Opens the vault with key material supplied by the caller.
    pub fn open(key: &VaultKey) -> Self {
        Self {
            cipher: Aes256Gcm::new(key),
            slots: HashMap::new(),
            next: 0,
        }
    }

    /// Seals `raw` and returns its handle. The nonce is derived from the slot number so
    /// sealing is deterministic under a fixed key for tests; handles never repeat.
    pub fn put(&mut self, raw: &[u8], reason: &str) -> Result<String, VaultError> {
        let slot = self.next;
        self.next += 1;
        let handle = format!("vault-{reason}-{slot}");
        let slot_bytes = slot.to_le_bytes();
        let nonce = Nonce::from_slice(&slot_bytes);
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
}
