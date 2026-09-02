//! The ingress transaction itself: capture -> redact -> segment -> append.
//!
//! The transaction is fail-closed. It owns a bounded volatile capture buffer, runs the
//! redactor, checks total disjoint byte coverage of the sanitized bytes, performs the
//! one exempt durable write (the sanitized append), and only then returns the ingress
//! digest. If the process dies between the sanitized append and item placement, replay
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
    /// Store is not in a writable mode: fail closed before any side effect.
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

/// One admitted ingress record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngressRecord {
    pub source: CaptureSource,
    pub sanitized: Vec<u8>,
    pub segments: Vec<Segment>,
    pub redactions: usize,
    pub vault: Option<VaultReference>,
    /// Digests of each quarantined secret token, so generated artifacts can be checked
    /// for laundering without storing plaintext anywhere durable.
    pub secret_digests: Vec<u64>,
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

/// A captured-but-not-yet-ingested generated artifact (volatile by contract).
pub struct PendingDerivation {
    pub source_kind: &'static str,
    pub raw: Vec<u8>,
}

/// Callback port the transaction uses to reach the store.
pub trait IngressSink {
    /// Appends sanitized bytes; returns the byte range they occupy in the spine.
    fn sanitized_append(&mut self, bytes: &[u8]) -> Result<Range<u64>, String>;
    /// Writes quarantined plaintext into the encrypted vault; returns its handle.
    fn vault_put(&mut self, raw: &[u8], reason: &str) -> Result<String, String>;
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

    /// Commits the transaction: redact, segment, coverage-check, append.
    pub fn commit(
        &mut self,
        sink: &mut dyn IngressSink,
    ) -> Result<Vec<IngressRecord>, IngressError> {
        if sink.mode() != "normal" {
            return Err(IngressError::StoreBlocked { mode: sink.mode() });
        }
        let slots = self.capture.drain();
        let mut records = Vec::new();
        for slot in slots {
            let record = self.transact_one(sink, slot.source, &slot.bytes)?;
            self.quarantine
                .extend(record.secret_digests.iter().copied());
            records.push(record);
        }
        Ok(records)
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
                let segments = segment(&bytes);
                if !coverage_is_total(&segments, bytes.len()) {
                    return Err(IngressError::Coverage {
                        sanitized_len: bytes.len(),
                    });
                }
                sink.sanitized_append(&bytes)
                    .map_err(|e| IngressError::StoreBlocked {
                        mode: leak_mode(&e),
                    })?;
                Ok(IngressRecord {
                    source,
                    sanitized: bytes,
                    segments,
                    redactions: redactions.len(),
                    vault: None,
                    secret_digests: secret_token_digests(raw),
                })
            }
            RedactionOutcome::Vaulted { reason, byte_len } => {
                let handle =
                    sink.vault_put(raw, reason.name())
                        .map_err(|e| IngressError::StoreBlocked {
                            mode: leak_mode(&e),
                        })?;
                let placeholder = vault_placeholder(reason.clone(), byte_len);
                let segments = segment(&placeholder);
                sink.sanitized_append(&placeholder)
                    .map_err(|e| IngressError::StoreBlocked {
                        mode: leak_mode(&e),
                    })?;
                Ok(IngressRecord {
                    source,
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
                })
            }
        }
    }
}

fn vault_placeholder(reason: VaultReason, byte_len: usize) -> Vec<u8> {
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

fn leak_mode(message: &str) -> &'static str {
    if message.contains("read-only") {
        "read-only"
    } else if message.contains("unavailable") {
        "unavailable"
    } else {
        "blocked"
    }
}
