//! Framed append-only transaction encoding and segment I/O.

use super::*;
use sha2::{Digest as _, Sha256};
use std::io::{Seek as _, Write as _};
use std::os::unix::fs::MetadataExt as _;

pub(super) const FORMAT_VERSION: u32 = 1;
pub(super) const SEGMENT_BYTE_THRESHOLD: u64 = 4 * 1024 * 1024;
pub(super) const EVENT_THRESHOLD: u64 = 1024;
const MAGIC: &[u8; 8] = b"LLXLOG01";
const HEADER_LEN: usize = 56;
const DIGEST_LEN: usize = 16;
const MAX_FRAME_PAYLOAD: usize = MAX_SESSION_BYTES + 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct EventBatch {
    pub txn_id: String,
    pub events: Vec<Event>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum Event {
    BranchReserved {
        cwd: Option<String>,
        cwd_dev: u64,
        cwd_ino: u64,
        branch: BranchRecord,
        next_branch_seq: u64,
    },
    BranchReclaimed {
        branch_id: String,
        prompt: String,
        owner: String,
        reserved_at: u64,
        lease_expiry: u64,
    },
    LeaseRenewed {
        branch_id: String,
        owner: String,
        lease_expiry: u64,
    },
    Checkpoint {
        branch_id: String,
        owner: String,
        rounds: Vec<RoundRecord>,
        lease_expiry: u64,
    },
    BranchCompleted {
        branch_id: String,
        owner: String,
        rounds: Vec<RoundRecord>,
        summary: String,
    },
    BranchFailed {
        branch_id: String,
        owner: String,
        rounds: Vec<RoundRecord>,
        error: String,
    },
}

pub(super) struct EncodedFrame {
    pub bytes: Vec<u8>,
    pub digest: [u8; DIGEST_LEN],
    pub batch: EventBatch,
}

pub(super) struct ReplayCursor {
    pub seq: u64,
    pub offset: u64,
    pub digest: [u8; DIGEST_LEN],
    pub events: u64,
}

pub(super) struct ReplayResult {
    pub cursor: ReplayCursor,
    pub repaired_tail: bool,
}

pub(super) fn full_digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub(super) fn digest_prefix(bytes: &[u8]) -> [u8; DIGEST_LEN] {
    let digest = Sha256::digest(bytes);
    let mut out = [0; DIGEST_LEN];
    out.copy_from_slice(&digest[..DIGEST_LEN]);
    out
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 15) as usize] as char);
    }
    out
}

fn txn_id(events: &[Event]) -> Result<String, StoreError> {
    let bytes = serde_json::to_vec(events)
        .map_err(|error| StoreError::Corrupt(format!("serialize transaction: {error}")))?;
    Ok(hex(&Sha256::digest(bytes)[..16]))
}

pub(super) fn encode_frame(
    seq: u64,
    previous: [u8; DIGEST_LEN],
    events: &[Event],
) -> Result<EncodedFrame, StoreError> {
    let txn_id = txn_id(events)?;
    let batch = EventBatch {
        txn_id: txn_id.clone(),
        events: events.to_vec(),
    };
    let payload = serde_json::to_vec(&batch)
        .map_err(|error| StoreError::Corrupt(format!("serialize transaction: {error}")))?;
    if payload.len() > MAX_FRAME_PAYLOAD {
        return Err(StoreError::Invalid(
            "session transaction exceeds the frame byte cap".into(),
        ));
    }
    let payload_len = u32::try_from(payload.len()).map_err(|_| {
        StoreError::Invalid("session transaction exceeds the frame byte cap".into())
    })?;
    let mut header = Vec::with_capacity(HEADER_LEN);
    header.extend_from_slice(MAGIC);
    header.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    header.extend_from_slice(&payload_len.to_le_bytes());
    header.extend_from_slice(&seq.to_le_bytes());
    let txn_bytes = Sha256::digest(txn_id.as_bytes());
    header.extend_from_slice(&txn_bytes[..16]);
    header.extend_from_slice(&previous);
    debug_assert_eq!(header.len(), HEADER_LEN);
    let mut bytes = header;
    bytes.extend_from_slice(&payload);
    let digest = digest_prefix(&bytes);
    bytes.extend_from_slice(&digest);
    Ok(EncodedFrame {
        bytes,
        digest,
        batch,
    })
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("fixed frame header"))
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("fixed frame header"))
}

fn corrupt(message: &str) -> StoreError {
    StoreError::Corrupt(message.to_string())
}

pub(super) fn replay_from(
    file: &mut std::fs::File,
    state: &mut SessionState,
    mut cursor: ReplayCursor,
    permit_tail_repair: bool,
) -> Result<ReplayResult, StoreError> {
    let bytes = read_replay_bytes(file, &cursor)?;
    let base_offset = cursor.offset;
    let mut at = 0usize;
    let mut repaired = false;
    while at < bytes.len() {
        let available = bytes.len() - at;
        if available < HEADER_LEN {
            if available <= MAGIC.len() && bytes[at..] != MAGIC[..available] {
                return Err(corrupt("garbage follows the final session frame"));
            }
            repaired = repair_tail(file, base_offset + at as u64, permit_tail_repair)?;
            break;
        }
        let header = &bytes[at..at + HEADER_LEN];
        let payload_len = validate_header(header, &cursor)?;
        let frame_len = HEADER_LEN
            .checked_add(payload_len)
            .and_then(|value| value.checked_add(DIGEST_LEN))
            .ok_or_else(|| corrupt("session frame length overflow"))?;
        if available < frame_len {
            repaired = repair_tail(file, base_offset + at as u64, permit_tail_repair)?;
            break;
        }
        let frame = &bytes[at..at + frame_len];
        let digest = validate_frame(frame, payload_len)?;
        let batch = decode_batch(frame, payload_len, header)?;
        super::replay::apply_batch(state, &batch)?;
        cursor.seq = u64_at(header, 16);
        cursor.digest = digest;
        cursor.events = cursor
            .events
            .checked_add(1)
            .ok_or_else(|| corrupt("session event count overflow"))?;
        at += frame_len;
        cursor.offset = cursor
            .offset
            .checked_add(frame_len as u64)
            .ok_or_else(|| corrupt("session segment offset overflow"))?;
    }
    state.validate()?;
    Ok(ReplayResult {
        cursor,
        repaired_tail: repaired,
    })
}

pub(super) const fn max_replay_bytes() -> u64 {
    SEGMENT_BYTE_THRESHOLD
        .saturating_add(MAX_FRAME_PAYLOAD as u64)
        .saturating_add((HEADER_LEN + DIGEST_LEN) as u64)
}

fn read_replay_bytes(
    file: &mut std::fs::File,
    cursor: &ReplayCursor,
) -> Result<Vec<u8>, StoreError> {
    let file_len = file
        .metadata()
        .map_err(|_| StoreError::Io("inspect session segment failed".into()))?
        .len();
    if cursor.offset > file_len {
        return Err(corrupt("session segment shrank"));
    }
    file.seek(std::io::SeekFrom::Start(cursor.offset))
        .map_err(|_| StoreError::Io("seek session segment failed".into()))?;
    let remaining = file_len - cursor.offset;
    if remaining > max_replay_bytes() {
        return Err(corrupt("active session segment exceeds replay bound"));
    }
    let mut bytes = Vec::new();
    file.take(remaining)
        .read_to_end(&mut bytes)
        .map_err(|_| StoreError::Io("read session segment failed".into()))?;
    Ok(bytes)
}

fn validate_header(header: &[u8], cursor: &ReplayCursor) -> Result<usize, StoreError> {
    if &header[..8] != MAGIC {
        return Err(corrupt("session frame has invalid magic"));
    }
    if u32_at(header, 8) != FORMAT_VERSION {
        return Err(corrupt("unsupported session frame version"));
    }
    let payload_len = u32_at(header, 12) as usize;
    if payload_len > MAX_FRAME_PAYLOAD {
        return Err(corrupt("session frame exceeds its byte cap"));
    }
    let expected_seq = cursor
        .seq
        .checked_add(1)
        .ok_or_else(|| corrupt("session sequence overflow"))?;
    if u64_at(header, 16) != expected_seq {
        return Err(corrupt("session frame sequence is not contiguous"));
    }
    if header[40..56] != cursor.digest {
        return Err(corrupt("session frame digest chain is broken"));
    }
    Ok(payload_len)
}

fn validate_frame(frame: &[u8], payload_len: usize) -> Result<[u8; DIGEST_LEN], StoreError> {
    let digest = digest_prefix(&frame[..HEADER_LEN + payload_len]);
    if frame[HEADER_LEN + payload_len..] != digest {
        return Err(corrupt("session frame digest mismatch"));
    }
    Ok(digest)
}

fn decode_batch(frame: &[u8], payload_len: usize, header: &[u8]) -> Result<EventBatch, StoreError> {
    let batch: EventBatch = serde_json::from_slice(&frame[HEADER_LEN..HEADER_LEN + payload_len])
        .map_err(|_| corrupt("session frame payload is invalid"))?;
    let expected_txn = txn_id(&batch.events).map_err(|_| corrupt("invalid transaction"))?;
    if batch.txn_id != expected_txn {
        return Err(corrupt("session transaction id mismatch"));
    }
    let txn_bytes = Sha256::digest(batch.txn_id.as_bytes());
    if header[24..40] != txn_bytes[..16] {
        return Err(corrupt("session frame transaction header mismatch"));
    }
    Ok(batch)
}

fn repair_tail(file: &mut std::fs::File, length: u64, permitted: bool) -> Result<bool, StoreError> {
    if !permitted {
        return Err(corrupt("retained session segment has an incomplete frame"));
    }
    file.set_len(length)
        .map_err(|_| StoreError::Io("truncate incomplete session frame failed".into()))?;
    file.sync_all()
        .map_err(|_| StoreError::Io("sync repaired session segment failed".into()))?;
    Ok(true)
}

pub(super) fn append_frame(
    dir: &openat::Dir,
    name: &str,
    expected_offset: u64,
    frame: &EncodedFrame,
) -> Result<(), StoreError> {
    let mut file = super::open_regular_at(dir, name, libc::O_RDWR, 0)
        .map_err(|_| StoreError::Io("open active session segment failed".into()))?;
    let installed = super::open_regular_at(dir, name, libc::O_RDONLY, 0)
        .map_err(|_| StoreError::Io("verify active session segment failed".into()))?;
    if !super::same_file_identity(&file, &installed)?
        || file.metadata().map(|m| m.len()).ok() != Some(expected_offset)
    {
        return Err(StoreError::Io(
            "active session segment was replaced or changed".into(),
        ));
    }
    file.seek(std::io::SeekFrom::Start(expected_offset))
        .map_err(|_| StoreError::Io("seek active session segment failed".into()))?;
    file.write_all(&frame.bytes).map_err(|_| {
        StoreError::Io(format!(
            "append session transaction {} failed",
            frame.batch.txn_id
        ))
    })?;
    file.sync_all().map_err(|_| {
        StoreError::Io(format!(
            "sync session transaction {} failed",
            frame.batch.txn_id
        ))
    })?;
    let installed = super::open_regular_at(dir, name, libc::O_RDONLY, 0)
        .map_err(|_| StoreError::Io("verify appended session segment failed".into()))?;
    let expected_len = expected_offset
        .checked_add(frame.bytes.len() as u64)
        .ok_or_else(|| StoreError::Corrupt("session segment length overflow".into()))?;
    let metadata = installed
        .metadata()
        .map_err(|_| StoreError::Io("inspect appended session segment failed".into()))?;
    if !super::same_file_identity(&file, &installed)? || metadata.len() != expected_len {
        return Err(StoreError::Io(
            "active session segment name was replaced".into(),
        ));
    }
    Ok(())
}

pub(super) fn identity(file: &std::fs::File) -> Result<(u64, u64), StoreError> {
    let metadata = file
        .metadata()
        .map_err(|_| StoreError::Io("inspect session segment failed".into()))?;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(test)]
mod tests;
