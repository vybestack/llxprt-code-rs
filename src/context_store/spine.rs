//! Append-only, byte-addressable sanitized spine with framed records.
//!
//! Records are framed `[u32 length][bytes][u64 digest]`. Loading validates every frame
//! and drops a corrupt tail, reporting how many records were recovered away. Reads are
//! range reads with bounded pages, so a caller can paginate without unbounded allocation.

use crate::context_kernel::canonical::digest;
use std::ops::Range;

/// One framed record in the spine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpineRecord {
    pub handle: String,
    pub range: Range<u64>,
    pub content_digest: u64,
}

/// One admitted sanitized record with its stored handle.
///
/// Handles are content-stable: `sanitized-<16 hex of frame digest>` where the
/// frame digest covers the payload and the preceding byte length, so the same
/// payload at the same spine position resolves to the same handle before and
/// after a reload (issue #102).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpineFrame {
    pub handle: String,
    pub bytes: Vec<u8>,
    pub content_digest: u64,
}

impl SpineFrame {
    /// The canonical content-stable handle for the frame that would start at
    /// byte offset `preceding_len`.
    pub fn canonical_handle(preceding_len: u64, bytes: &[u8]) -> String {
        let mut material = Vec::with_capacity(bytes.len() + 8);
        material.extend_from_slice(&preceding_len.to_le_bytes());
        material.extend_from_slice(bytes);
        format!("sanitized-{:016x}", digest(&material))
    }
}

/// Errors raised by typed framed loading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpineLoadError {
    /// A frame is truncated, has a bad length, or fails its digest.
    CorruptFrame {
        /// Zero-based index of the first bad frame.
        index: usize,
        /// How many frames validated before the failure.
        good_records: usize,
    },
}

/// Errors raised by the spine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpineError {
    RangeOutside { start: u64, end: u64, len: u64 },
    CorruptTail(SpineLoadError),
}

/// In-memory append-only spine with framing and corrupt-tail recovery.
pub struct Spine {
    bytes: Vec<u8>,
    records: Vec<SpineRecord>,
    recovered_tails: usize,
}

/// One bounded page of a range read.
pub struct Page {
    pub bytes: Vec<u8>,
    /// Range still unread, when the page hit the bound.
    pub remaining: Option<Range<u64>>,
}

impl Spine {
    /// Creates an empty spine.
    pub fn new() -> Self {
        Self {
            bytes: Vec::new(),
            records: Vec::new(),
            recovered_tails: 0,
        }
    }

    /// Total bytes in the spine.
    pub fn len(&self) -> u64 {
        self.bytes.len() as u64
    }

    /// Whether the spine is empty.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Records appended so far.
    pub fn records(&self) -> &[SpineRecord] {
        &self.records
    }

    /// Corrupt tails dropped by the last load.
    pub fn recovered_tail_records(&self) -> usize {
        self.recovered_tails
    }

    /// Appends sanitized bytes; the spine is append-only, so no range ever moves.
    pub fn append(&mut self, handle: &str, bytes: &[u8]) -> Range<u64> {
        let start = self.bytes.len() as u64;
        self.bytes.extend_from_slice(bytes);
        let range = start..(self.bytes.len() as u64);
        self.records.push(SpineRecord {
            handle: handle.to_string(),
            range: range.clone(),
            content_digest: digest(bytes),
        });
        range
    }

    /// Reads one bounded page of `range`.
    pub fn read_page(&self, range: Range<u64>, limit: usize) -> Result<Page, SpineError> {
        let len = self.len();
        if range.start > range.end || range.end > len {
            return Err(SpineError::RangeOutside {
                start: range.start,
                end: range.end,
                len,
            });
        }
        let wanted = (range.end - range.start) as usize;
        let take = wanted.min(limit);
        let start = range.start as usize;
        let bytes = self.bytes[start..start + take].to_vec();
        let remaining = if take < wanted {
            Some((range.start + take as u64)..range.end)
        } else {
            None
        };
        Ok(Page { bytes, remaining })
    }

    /// Reads a full range, refusing ranges larger than `max`.
    pub fn read(&self, range: Range<u64>, max: usize) -> Result<Vec<u8>, SpineError> {
        let page = self.read_page(range.clone(), max)?;
        if page.remaining.is_some() {
            return Err(SpineError::RangeOutside {
                start: range.start,
                end: range.end,
                len: self.len(),
            });
        }
        Ok(page.bytes)
    }

    /// Encodes every record with framing.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for record in &self.records {
            let bytes = &self.bytes[record.range.start as usize..record.range.end as usize];
            out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            out.extend_from_slice(bytes);
            out.extend_from_slice(&record.content_digest.to_le_bytes());
        }
        out
    }

    /// Loads framed records, failing closed with the index of the first
    /// corrupt frame instead of silently truncating the spine (issue #102).
    pub fn load_typed(encoded: &[u8]) -> Result<Self, SpineLoadError> {
        let mut spine = Spine::new();
        let mut cursor = 0usize;
        let mut index = 0usize;
        while cursor < encoded.len() {
            let Some(record) = frame_at(encoded, cursor) else {
                return Err(SpineLoadError::CorruptFrame {
                    index,
                    good_records: spine.records.len(),
                });
            };
            let (bytes, content_digest, next) = record;
            if digest(bytes) != content_digest {
                return Err(SpineLoadError::CorruptFrame {
                    index,
                    good_records: spine.records.len(),
                });
            }
            let handle = SpineFrame::canonical_handle(spine.len(), bytes);
            spine.append(&handle, bytes);
            cursor = next;
            index += 1;
        }
        Ok(spine)
    }

    /// Loads framed records, dropping a corrupt tail instead of failing the whole spine.
    ///
    /// In-process salvage path; the durable loader is [`Spine::load_typed`].
    pub fn load(encoded: &[u8]) -> Self {
        let mut spine = Spine::new();
        let mut cursor = 0usize;
        while cursor < encoded.len() {
            let Some(record) = frame_at(encoded, cursor) else {
                break;
            };
            let (bytes, content_digest, next) = record;
            if digest(bytes) != content_digest {
                break;
            }
            let handle = SpineFrame::canonical_handle(spine.len(), bytes);
            spine.append(&handle, bytes);
            cursor = next;
        }
        spine.recovered_tails = usize::from(cursor != encoded.len());
        spine
    }
}

fn frame_at(encoded: &[u8], cursor: usize) -> Option<(&[u8], u64, usize)> {
    if cursor + 4 > encoded.len() {
        return None;
    }
    let length = u32::from_le_bytes(encoded[cursor..cursor + 4].try_into().ok()?) as usize;
    let body = cursor + 4;
    if body + length + 8 > encoded.len() {
        return None;
    }
    let bytes = &encoded[body..body + length];
    let digest_at = body + length;
    let content_digest = u64::from_le_bytes(encoded[digest_at..digest_at + 8].try_into().ok()?);
    Some((bytes, content_digest, digest_at + 8))
}

impl Default for Spine {
    fn default() -> Self {
        Self::new()
    }
}
