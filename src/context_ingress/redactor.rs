//! Redactor: the system-authority detector set that runs inside the ingress transaction.
//!
//! Detectors are classed over the leak corpus. Matching spans are replaced in place with
//! the same byte length, so every byte outside a redacted span keeps its offset and any
//! segmentation computed over the redacted bytes is stable. A detector that exhausts its
//! byte work budget or fails outright routes the whole payload to the encrypted vault:
//! the sanitized spine then carries only a vault reference. The redactor never fails
//! open.

use crate::context_kernel::canonical::digest;
use std::ops::Range;

/// Detector class over the leak corpus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectorClass {
    CorpusMarker,
    CredentialToken,
    UrlCredential,
    BearerToken,
    AssignSecret,
}

impl DetectorClass {
    /// Stable name for reports and vault references.
    pub fn name(self) -> &'static str {
        match self {
            DetectorClass::CorpusMarker => "corpus-marker",
            DetectorClass::CredentialToken => "credential-token",
            DetectorClass::UrlCredential => "url-credential",
            DetectorClass::BearerToken => "bearer-token",
            DetectorClass::AssignSecret => "assign-secret",
        }
    }
}

/// One matched secret span in the raw payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detection {
    pub class: DetectorClass,
    pub span: Range<usize>,
}

/// Verdict of one detector pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanVerdict {
    Clean,
    Detected(Vec<Detection>),
    /// The work budget ran out before the scan finished: fail closed to the vault.
    BudgetExhausted,
    /// The detector itself failed: fail closed to the vault.
    Failed,
}

/// One classed detector with its own byte work budget.
pub struct Detector {
    class: DetectorClass,
    markers: Vec<&'static str>,
    budget: usize,
    fail: bool,
}

impl Detector {
    /// Creates a working detector for `class` triggered by `markers`.
    pub fn new(class: DetectorClass, markers: Vec<&'static str>, budget: usize) -> Self {
        Self {
            class,
            markers,
            budget,
            fail: false,
        }
    }

    /// Creates a detector that always fails, for fault injection and tests.
    pub fn failing(class: DetectorClass) -> Self {
        Self {
            class,
            markers: Vec::new(),
            budget: usize::MAX,
            fail: true,
        }
    }

    /// Scans `bytes`, consuming the work budget byte by byte.
    pub fn scan(&self, bytes: &[u8]) -> ScanVerdict {
        if self.fail {
            return ScanVerdict::Failed;
        }
        let mut budget = self.budget;
        let mut detections: Vec<Detection> = Vec::new();
        let mut index = 0usize;
        while index < bytes.len() {
            if budget == 0 {
                return ScanVerdict::BudgetExhausted;
            }
            budget -= 1;
            let marker = self.matching_marker(bytes, index);
            match marker {
                Some(marker) => {
                    let start = index;
                    let mut end = index + marker.len();
                    while end < bytes.len() && !is_delimiter(bytes[end]) {
                        end += 1;
                    }
                    detections.push(Detection {
                        class: self.class,
                        span: start..end,
                    });
                    index = end;
                }
                None => index += 1,
            }
        }
        if detections.is_empty() {
            ScanVerdict::Clean
        } else {
            ScanVerdict::Detected(detections)
        }
    }

    fn matching_marker(&self, bytes: &[u8], at: usize) -> Option<&'static str> {
        self.markers
            .iter()
            .copied()
            .find(|marker| matches_at(bytes, at, marker))
    }
}

fn matches_at(bytes: &[u8], at: usize, marker: &str) -> bool {
    let needle = marker.as_bytes();
    if at + needle.len() > bytes.len() {
        return false;
    }
    &bytes[at..at + needle.len()] == needle
}

fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b' ' | b'\n' | b'\r' | b'\t' | b'"' | b'\'' | b')' | b',' | b';' | b'>' | b'<'
    )
}

/// Why a payload was routed to the encrypted vault instead of the sanitized spine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultReason {
    BudgetExhausted { class: DetectorClass },
    DetectorFailed { class: DetectorClass },
}

impl VaultReason {
    /// Stable name for the vault reference stored in the sanitized spine.
    pub fn name(&self) -> &'static str {
        match self {
            VaultReason::BudgetExhausted { .. } => "detector-budget-exhausted",
            VaultReason::DetectorFailed { .. } => "detector-failed",
        }
    }
}

/// Outcome of redaction: sanitized bytes, or a whole-payload vault quarantine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedactionOutcome {
    Sanitized {
        bytes: Vec<u8>,
        redactions: Vec<Detection>,
    },
    Vaulted {
        reason: VaultReason,
        byte_len: usize,
    },
}

/// Default generous per-detector work budget (bytes examined per detector).
pub const DEFAULT_WORK_BUDGET: usize = 1 << 20;

/// The redactor: every detector runs; any fail-closed verdict wins.
pub struct Redactor {
    detectors: Vec<Detector>,
}

impl Redactor {
    /// Builds the default detector set with one shared work budget.
    pub fn with_budget(work_budget: usize) -> Self {
        Self {
            detectors: vec![
                Detector::new(
                    DetectorClass::CorpusMarker,
                    vec!["CTXEVAL-SECRET-", "CTXEVAL-TOKEN-"],
                    work_budget,
                ),
                Detector::new(
                    DetectorClass::CredentialToken,
                    vec!["sk-ctxeval", "auth-key-"],
                    work_budget,
                ),
                Detector::new(
                    DetectorClass::UrlCredential,
                    vec!["ctxeval-creds@", "://secret:"],
                    work_budget,
                ),
                Detector::new(
                    DetectorClass::BearerToken,
                    vec!["Bearer CTXEVAL-"],
                    work_budget,
                ),
                Detector::new(
                    DetectorClass::AssignSecret,
                    vec!["password=CTXEVAL-", "api_key=CTXEVAL-"],
                    work_budget,
                ),
            ],
        }
    }

    /// Builds a redactor from an explicit detector set (tests and fault injection).
    pub fn from_detectors(detectors: Vec<Detector>) -> Self {
        Self { detectors }
    }

    /// Redacts `raw`, replacing matched spans byte-for-byte, or quarantines to vault.
    pub fn redact(&self, raw: &[u8]) -> RedactionOutcome {
        let mut bytes = raw.to_vec();
        let mut redactions: Vec<Detection> = Vec::new();
        for detector in &self.detectors {
            match detector.scan(raw) {
                ScanVerdict::Clean => {}
                ScanVerdict::Detected(detections) => {
                    for detection in detections {
                        overwrite(&mut bytes, &detection.span);
                        redactions.push(detection);
                    }
                }
                ScanVerdict::BudgetExhausted => {
                    return RedactionOutcome::Vaulted {
                        reason: VaultReason::BudgetExhausted {
                            class: detector.class,
                        },
                        byte_len: raw.len(),
                    }
                }
                ScanVerdict::Failed => {
                    return RedactionOutcome::Vaulted {
                        reason: VaultReason::DetectorFailed {
                            class: detector.class,
                        },
                        byte_len: raw.len(),
                    }
                }
            }
        }
        RedactionOutcome::Sanitized { bytes, redactions }
    }
}

/// Replaces `span` with the same number of `X` bytes so every other offset is stable.
fn overwrite(bytes: &mut [u8], span: &Range<usize>) {
    for slot in bytes.iter_mut().skip(span.start).take(span.len()) {
        *slot = b'X';
    }
}

/// Digest of a secret value, used by the laundering ledger without storing plaintext.
pub fn secret_digest(secret: &[u8]) -> u64 {
    digest(secret)
}
