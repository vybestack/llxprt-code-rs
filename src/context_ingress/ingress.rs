//! The ingress transaction itself: capture -> redact -> coverage-check -> append -> segment.
//!
//! The transaction is fail-closed. It owns a bounded volatile capture buffer, runs the
//! redactor, checks total disjoint byte coverage of the sanitized bytes for every slot
//! BEFORE any durable write (so a rejected payload leaves the spine byte-identical),
//! performs the one exempt durable write (the sanitized append), and only then returns the
//! ingress digest. If the process dies between the sanitized append and item placement, replay
//! re-derives segmentation and classification deterministically from the stored bytes.
//! Generated artifacts enter through the same path as a synchronous sub-transaction and
//! stay volatile until it completes.

use crate::context_ingress::capture::{CaptureBuffer, CaptureError, CaptureSource};
use crate::context_ingress::redactor::{RedactionOutcome, Redactor, VaultReason};
use crate::context_ingress::segment::{coverage_is_total, segment, Segment};
use crate::context_kernel::canonical::{digest, Digest};
use std::ops::Range;

/// Reason ingress refused to admit a payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngressError {
    Capture(CaptureError),
    /// Segmentation did not cover the sanitized bytes exactly: refuse admission.
    Coverage {
        sanitized_len: usize,
    },
    /// A record that preserved zero spine bytes (an empty sanitized/placeholder
    /// range) is refused admission: an exempt append must land real bytes, and an
    /// empty placement would otherwise be admitted with a zero-length spine
    /// range that preserves nothing.
    EmptyPreservedSpine,
    /// The sink refused a durable write: `mode` names what refused it. For a
    /// store-mode refusal it is the store's own mode name (`read-only`,
    /// `unavailable`), carried through the sink typed rather than recovered
    /// from a rendered message; a vault refusal names the vault.
    StoreBlocked {
        mode: &'static str,
    },
}

/// A vault reference recorded in the sanitized spine instead of raw content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultReference {
    /// Vault slot identity.
    pub handle: String,
    /// Why the content was quarantined.
    pub reason: String,
    /// Sanitized placeholder bytes stored in the spine (byte-length stable).
    pub placeholder: Vec<u8>,
    /// Digest of the quarantined plaintext for audit (never the plaintext).
    pub content_digest: Digest,
}

/// Where the transaction's exempt append landed in the sanitized spine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpinePlacement {
    /// Content-stable handle the sink assigned to the record.
    pub handle: String,
    /// Byte range the record occupies in the spine.
    pub range: Range<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngressRecord {
    pub source: CaptureSource,
    /// The admitted record as it landed in the store: handle, spine ranges,
    /// admitted bytes, and segmentation.
    pub payload: IngressPayload,
    pub sanitized: Vec<u8>,
    pub segments: Vec<Segment>,
    pub redactions: usize,
    pub vault: Option<VaultReference>,
    /// Digests of each quarantined secret token, so generated artifacts can be checked
    /// for laundering without storing plaintext anywhere durable.
    pub secret_digests: Vec<u64>,
    /// Where the sanitized (or placeholder) bytes were appended. This is the
    /// transaction's own exempt write; no other spine write may exist for the
    /// payload (issue #100).
    pub spine: Option<SpinePlacement>,
}

impl IngressRecord {
    /// Ingress digest over sanitized bytes and provenance.
    pub fn digest(&self) -> Digest {
        let mut body = Vec::new();
        body.extend_from_slice(self.source.name().as_bytes());
        body.extend_from_slice(b"\x00");
        body.extend_from_slice(&self.sanitized);
        digest(&body)
    }

    /// Ranges that must be preserved verbatim.
    pub fn exact_spans(&self) -> Vec<Range<usize>> {
        self.segments
            .iter()
            .filter(|segment| {
                matches!(
                    segment.class,
                    crate::context_ingress::segment::StructuralClass::ExactSpan
                        | crate::context_ingress::segment::StructuralClass::Identifier
                )
            })
            .map(|segment| segment.span.clone())
            .collect()
    }
}

/// The admitted payload of one ingress record, as it landed in the store:
/// the sink-assigned handle, the spine ranges it covers, the admitted bytes,
/// and the segmentation of those bytes. Everything downstream (filter
/// digests, IR placement, read-back) consumes this view instead of the raw
/// input, so no unredacted bytes ever leave the transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IngressPayload {
    /// Content-stable handle the sink assigned to the admitted record.
    pub handle: String,
    /// Spine ranges the admitted record occupies.
    pub ranges: Vec<Range<u64>>,
    /// Admitted bytes exactly as they were appended to the spine.
    pub bytes: Vec<u8>,
    /// Segmentation of `bytes`, computed after the durable append.
    pub segments: Vec<Segment>,
}

/// A captured-but-not-yet-ingested generated artifact (volatile by contract).
pub struct PendingDerivation {
    pub source_kind: &'static str,
    pub raw: Vec<u8>,
}

/// Why the sink refused a durable write.
///
/// Typed, not rendered: the sink reports the store's own mode name so the
/// transaction fails fast on a value it can name, instead of recovering a mode
/// by matching substrings inside an error string. `Mode` carries the store's own
/// `&'static str` name, so `read-only` and `unavailable` are exactly the names
/// the store reports; `Vault` is a vault refusal under a writable store mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SinkRefusal {
    /// The store's mode refused the durable write.
    Mode { mode: &'static str },
    /// The vault refused the quarantined write.
    Vault,
}

/// Callback port the transaction uses to reach the store.
pub trait IngressSink {
    /// Appends sanitized bytes and returns where they landed: the
    /// sink-assigned content-stable handle plus the spine byte range.
    fn sanitized_append(&mut self, bytes: &[u8]) -> Result<SpinePlacement, SinkRefusal>;
    /// Writes quarantined plaintext into the encrypted vault; returns its handle.
    fn vault_put(&mut self, raw: &[u8], reason: &str) -> Result<String, SinkRefusal>;
    /// Current store mode name, used to refuse writes before any side effect.
    fn mode(&self) -> &'static str;
}

/// The ingress transaction.
pub struct IngressTxn {
    capture: CaptureBuffer,
    redactor: Redactor,
    quarantine: Vec<u64>,
}

/// Number of records admitted and bytes appended by one transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngressStats {
    pub records: usize,
    pub sanitized_bytes: usize,
    pub vaulted: usize,
}

impl IngressTxn {
    /// Creates a transaction with a capture cap and a per-detector work budget.
    pub fn new(capture_cap: usize, work_budget: usize) -> Self {
        Self {
            capture: CaptureBuffer::new(capture_cap),
            redactor: Redactor::with_budget(work_budget),
            quarantine: Vec::new(),
        }
    }

    /// Declared loss if the process dies now.
    pub fn declared_loss(&self) -> usize {
        self.capture.at_risk()
    }

    /// Digests of every quarantined secret token seen by this transaction.
    pub fn quarantine_digests(&self) -> &[u64] {
        &self.quarantine
    }

    /// Captures an external payload into the volatile buffer.
    pub fn capture(&mut self, source: CaptureSource, bytes: &[u8]) -> Result<(), IngressError> {
        self.capture
            .push(source, bytes)
            .map_err(IngressError::Capture)
    }

    /// Captures a generated artifact for derivation-ingestion (volatile until admitted).
    pub fn capture_derivation(
        &mut self,
        _source_kind: &'static str,
        raw: &[u8],
    ) -> Result<(), IngressError> {
        self.capture(CaptureSource::GeneratedArtifact, raw)
    }

    /// Commits the transaction: redact, coverage-check, append, then segment.
    pub fn commit(
        &mut self,
        sink: &mut dyn IngressSink,
    ) -> Result<Vec<IngressRecord>, IngressError> {
        store_is_admitting(sink)?;
        // Validate every slot first, so a rejected payload leaves the spine
        // byte-identical to before the attempt (all-or-nothing admission):
        // a coverage failure in any slot appends nothing at all.
        let slots = self.capture.drain();
        for slot in slots.iter() {
            self.validate_slot(&slot.bytes)?;
        }
        let mut records = Vec::new();
        for slot in slots {
            let record = self.transact_one(sink, slot.source, &slot.bytes)?;
            self.quarantine
                .extend(record.secret_digests.iter().copied());
            records.push(record);
        }
        // Every slot has now landed durably. The mode checked at entry says
        // nothing about the store's state after the last exempt append, so the
        // transaction refuses to report success against a store that stopped
        // admitting writes mid-flight (issue 129).
        store_is_admitting(sink)?;
        Ok(records)
    }

    /// Pure validation of one slot's sanitized bytes: the coverage check runs
    /// BEFORE the durable append, so a rejected payload never leaves bytes in
    /// the spine (issue 107 regression).
    fn validate_slot(&self, raw: &[u8]) -> Result<(), IngressError> {
        match self.redactor.redact(raw) {
            RedactionOutcome::Sanitized { bytes, .. } => {
                if bytes.is_empty() {
                    return Err(IngressError::EmptyPreservedSpine);
                }
                let segments = segment(&bytes);
                if !coverage_is_total(&segments, bytes.len()) {
                    return Err(IngressError::Coverage {
                        sanitized_len: bytes.len(),
                    });
                }
                Ok(())
            }
            RedactionOutcome::Vaulted { reason, byte_len } => {
                let placeholder = vault_placeholder(reason, byte_len);
                if placeholder.is_empty() {
                    return Err(IngressError::EmptyPreservedSpine);
                }
                let segments = segment(&placeholder);
                if !coverage_is_total(&segments, placeholder.len()) {
                    return Err(IngressError::Coverage {
                        sanitized_len: placeholder.len(),
                    });
                }
                Ok(())
            }
        }
    }

    fn transact_one(
        &self,
        sink: &mut dyn IngressSink,
        source: CaptureSource,
        raw: &[u8],
    ) -> Result<IngressRecord, IngressError> {
        let outcome = self.redactor.redact(raw);
        match outcome {
            RedactionOutcome::Sanitized { bytes, redactions } => {
                let (placement, segments) = Self::place_on_spine(sink, &bytes)?;
                let payload = IngressPayload {
                    handle: placement.handle.clone(),
                    ranges: vec![placement.range.clone()],
                    bytes: bytes.clone(),
                    segments: segments.clone(),
                };
                Ok(IngressRecord {
                    source,
                    payload,
                    sanitized: bytes,
                    segments,
                    redactions: redactions.len(),
                    vault: None,
                    secret_digests: secret_token_digests(raw),
                    spine: Some(placement),
                })
            }
            RedactionOutcome::Vaulted { reason, byte_len } => {
                // The placeholder depends only on the quarantine reason and the
                // raw byte length, never on where the spine places it, so it
                // is built and landed on the spine BEFORE the vault write: a
                // spine refusal must not leave already-durable raw plaintext
                // in the vault with no spine reference naming it (issue 130).
                let placeholder = vault_placeholder(reason.clone(), byte_len);
                let (placement, segments) = Self::place_on_spine(sink, &placeholder)?;
                let handle = sink.vault_put(raw, reason.name()).map_err(|refusal| {
                    IngressError::StoreBlocked {
                        mode: sink_refusal_mode(&refusal),
                    }
                })?;
                let payload = IngressPayload {
                    handle: placement.handle.clone(),
                    ranges: vec![placement.range.clone()],
                    bytes: placeholder.clone(),
                    segments: segments.clone(),
                };
                Ok(IngressRecord {
                    source,
                    payload,
                    sanitized: placeholder.clone(),
                    segments,
                    redactions: 0,
                    secret_digests: secret_token_digests(raw),
                    vault: Some(VaultReference {
                        handle,
                        reason: reason.name().to_string(),
                        placeholder,
                        content_digest: digest(raw),
                    }),
                    spine: Some(placement),
                })
            }
        }
    }

    /// Appends `bytes` to the spine, then re-checks store state and the placement.
    ///
    /// `commit` already ran the pure validation pass over every slot and checked
    /// the store mode before any side effect, so this helper can assume the bytes
    /// are admissible; it still refuses, after the fact, a store that changed
    /// state during the append, a placement that landed zero spine bytes, or a
    /// segmentation that does not cover the stored bytes exactly. That
    /// post-append re-check is a belt-and-braces refusal, not an all-or-nothing
    /// guarantee: the exempt append is already durable when it fires, so by this
    /// point a rejected payload has left its bytes in the spine and the caller
    /// must fail the turn rather than roll the spine back. The pre-append
    /// validation in `commit` is what actually keeps the spine byte-identical
    /// for a rejected payload.
    fn place_on_spine(
        sink: &mut dyn IngressSink,
        bytes: &[u8],
    ) -> Result<(SpinePlacement, Vec<Segment>), IngressError> {
        let placement =
            sink.sanitized_append(bytes)
                .map_err(|refusal| IngressError::StoreBlocked {
                    mode: sink_refusal_mode(&refusal),
                })?;
        // The mode was checked before the append, but a store that flips to a
        // non-writable state during the append leaves the placement's validity
        // unproven, so the durable state is re-checked after the write.
        store_is_admitting(sink)?;
        if placement.range.is_empty() {
            return Err(IngressError::EmptyPreservedSpine);
        }
        let segments = segment(bytes);
        if !coverage_is_total(&segments, bytes.len()) {
            return Err(IngressError::Coverage {
                sanitized_len: bytes.len(),
            });
        }
        Ok((placement, segments))
    }
}

/// Builds the byte-length-stable placeholder the sanitized spine records for a payload
/// the redactor quarantined into the vault.
///
/// Shared by the ingress transaction's vaulted branch and the store-free digest fallback,
/// so both paths emit the same placeholder bytes for the same quarantine (issue 130).
pub(crate) fn vault_placeholder(reason: VaultReason, byte_len: usize) -> Vec<u8> {
    let label = format!("[vaulted:{}:{}bytes]", reason.name(), byte_len);
    let mut placeholder = label.into_bytes();
    placeholder.resize(byte_len.max(1), b'-');
    placeholder
}
/// Digests every delimiter-delimited token of `raw` so a quarantined secret cannot be
/// copied into a generated artifact without matching one of these digests.
fn secret_token_digests(raw: &[u8]) -> Vec<u64> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for index in 0..=raw.len() {
        let boundary = index == raw.len() || is_separator(raw[index]);
        if boundary {
            if index > start && index - start >= 8 {
                out.push(digest(&raw[start..index]));
            }
            start = index + 1;
        }
    }
    out
}

fn is_separator(byte: u8) -> bool {
    matches!(
        byte,
        b' ' | b'\n' | b'\r' | b'\t' | b'"' | b'\'' | b')' | b',' | b';'
    )
}

/// The store mode name a sink refusal implies, typed from the refusal itself.
///
/// A mode refusal carries the store's own mode name straight through, so the
/// transaction never guesses a mode back out of a rendered message. A vault
/// refusal is not a mode change: the store reported itself writable, so the
/// refusal names the sink's vault state and the caller fails fast on it.
fn sink_refusal_mode(refusal: &SinkRefusal) -> &'static str {
    match refusal {
        SinkRefusal::Mode { mode } => mode,
        SinkRefusal::Vault => "vault",
    }
}

/// Refuses unless the sink still reports a store that admits writes.
///
/// Used before the first durable call and again after every durable write: a store
/// that moved to `read-only` or `unavailable` mid-transaction must not have the
/// transaction's records reported as successfully admitted against it.
fn store_is_admitting(sink: &dyn IngressSink) -> Result<(), IngressError> {
    let mode = sink.mode();
    if mode == "normal" {
        Ok(())
    } else {
        Err(IngressError::StoreBlocked { mode })
    }
}
