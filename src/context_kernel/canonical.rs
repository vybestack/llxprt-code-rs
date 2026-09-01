//! Canonical, deterministic byte encoding for event checksums and state hashes.
//!
//! Every producer of a digest in the context kernel encodes through this module so
//! that replay of a recorded event prefix yields byte-identical typed state and an
//! identical hash. The encoding is append-only and field order is part of the
//! contract: adding a field requires a new schema version, never a reordering.

/// FNV-1a 64-bit offset basis.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// A 64-bit digest over canonical bytes.
pub type Digest = u64;

/// Computes the FNV-1a digest of `bytes`.
pub fn digest(bytes: &[u8]) -> Digest {
    let mut hash = FNV_OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Chains `previous` into a fresh digest over `bytes`, so each event checksum
/// commits to its predecessor and a rewritten prefix is detectable.
pub fn chained(previous: Digest, bytes: &[u8]) -> Digest {
    let mut buffer = previous.to_le_bytes().to_vec();
    buffer.extend_from_slice(bytes);
    digest(&buffer)
}

/// Append-only canonical encoder for one record.
#[derive(Default)]
pub struct Sink {
    buffer: Vec<u8>,
}

impl Sink {
    /// Creates an empty sink.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a length-prefixed tag naming the record or variant that follows.
    pub fn tag(&mut self, value: &str) {
        self.blob(value.as_bytes());
    }

    /// Appends an unsigned integer in little-endian order.
    pub fn int(&mut self, value: u64) {
        self.buffer.extend_from_slice(&value.to_le_bytes());
    }

    /// Appends a boolean as a single byte.
    pub fn flag(&mut self, value: bool) {
        self.buffer.push(u8::from(value));
    }

    /// Appends a length-prefixed byte string.
    pub fn blob(&mut self, value: &[u8]) {
        self.int(value.len() as u64);
        self.buffer.extend_from_slice(value);
    }

    /// Consumes the sink and returns the canonical bytes.
    pub fn finish(self) -> Vec<u8> {
        self.buffer
    }
}
