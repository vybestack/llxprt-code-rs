//! Red-first tests for the Phase 2 ingress pipeline (issue #39).
use crate::context_ingress::capture::{CaptureBuffer, CaptureLoss, CaptureSource};
use crate::context_ingress::filter::{
    Digest as FilterDigest, FilterClass, FilterRegistry, FilterRules, RuleVerdict, Vocabulary,
};
use crate::context_ingress::ingress::{IngressError, IngressRecord, IngressSink, IngressTxn};
use crate::context_ingress::launder::{LaunderVerdict, QuarantineSet};
use crate::context_ingress::redactor::{
    Detector, DetectorClass, RedactionOutcome, Redactor, ScanVerdict, VaultReason,
};
use crate::context_ingress::segment::{coverage_is_total, segment, StructuralClass};
use std::ops::Range;

/// In-memory sink used by the transaction tests.
struct MemSink {
    mode: &'static str,
    spine: Vec<u8>,
    vault: Vec<(String, Vec<u8>)>,
    fail_vault: bool,
}

impl MemSink {
    fn normal() -> Self {
        Self {
            mode: "normal",
            spine: Vec::new(),
            vault: Vec::new(),
            fail_vault: false,
        }
    }
}

impl IngressSink for MemSink {
    fn sanitized_append(&mut self, bytes: &[u8]) -> Result<Range<u64>, String> {
        if self.mode != "normal" {
            return Err(format!("store mode {} refuses append", self.mode));
        }
        let start = self.spine.len() as u64;
        self.spine.extend_from_slice(bytes);
        Ok(start..(self.spine.len() as u64))
    }

    fn vault_put(&mut self, raw: &[u8], reason: &str) -> Result<String, String> {
        if self.fail_vault {
            return Err(format!("vault unavailable while {reason}"));
        }
        let handle = format!("vault-{}", self.vault.len());
        self.vault.push((reason.to_string(), raw.to_vec()));
        Ok(handle)
    }

    fn mode(&self) -> &'static str {
        self.mode
    }
}

fn corpus() -> Vec<u8> {
    b"marker: CTXEVAL-SECRET-A1B2C3D4E5\nexact error span: bytes 4096..4131 \"unexpected trailing frame\"\nunknown-shaped identifier: x-txn-9f31ac04be\nnoise: fill line 0000\n".to_vec()
}

#[test]
fn redactor_replaces_each_detector_class_without_shifting_bytes() {
    let redactor = Redactor::with_budget(1 << 20);
    let raw = corpus();
    let outcome = redactor.redact(&raw);
    let RedactionOutcome::Sanitized { bytes, redactions } = outcome else {
        panic!("corpus must sanitize, not vault");
    };
    assert_eq!(bytes.len(), raw.len(), "structure must be preserved");
    let classes: Vec<_> = redactions.iter().map(|d| d.class).collect();
    assert!(
        classes.contains(&DetectorClass::CorpusMarker),
        "marker class missing"
    );
    assert!(
        !bytes
            .windows(13)
            .any(|window| window == b"CTXEVAL-SECRET".as_slice()),
        "secret survived redaction"
    );
    assert!(
        bytes
            .windows("unexpected trailing frame".len())
            .any(|window| window == b"unexpected trailing frame".as_slice()),
        "exact span must survive redaction verbatim"
    );
    assert!(coverage_is_total(&segment(&bytes), bytes.len()));
}

#[test]
fn detector_timeout_routes_whole_payload_to_vault() {
    let redactor = Redactor::from_detectors(vec![Detector::new(
        DetectorClass::CorpusMarker,
        vec!["CTXEVAL-SECRET-"],
        4,
    )]);
    let outcome = redactor.redact(&corpus());
    match outcome {
        RedactionOutcome::Vaulted { reason, byte_len } => {
            assert_eq!(
                reason,
                VaultReason::BudgetExhausted {
                    class: DetectorClass::CorpusMarker
                }
            );
            assert_eq!(byte_len, corpus().len());
        }
        other => panic!("expected vault, got {other:?}"),
    }
}

#[test]
fn detector_failure_routes_whole_payload_to_vault() {
    let redactor = Redactor::from_detectors(vec![Detector::failing(DetectorClass::BearerToken)]);
    match redactor.redact(b"clean bytes") {
        RedactionOutcome::Vaulted { reason, .. } => assert_eq!(
            reason,
            VaultReason::DetectorFailed {
                class: DetectorClass::BearerToken
            }
        ),
        other => panic!("expected vault, got {other:?}"),
    }
}

#[test]
fn vaulted_payload_leaves_only_a_reference_in_the_spine() {
    let mut sink = MemSink::normal();
    sink.fail_vault = false;
    let mut txn = IngressTxn::new(1 << 20, 1);
    txn.capture(CaptureSource::ToolResult, &corpus()).unwrap();
    let records = txn.commit(&mut sink).unwrap();
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert!(record.vault.is_some(), "tiny budget must force vault");
    let vault = record.vault.as_ref().unwrap();
    assert!(vault.handle.starts_with("vault-"));
    assert_eq!(vault.placeholder.len(), corpus().len());
    assert!(!String::from_utf8_lossy(&record.sanitized).contains("CTXEVAL-SECRET"));
    assert_eq!(sink.vault.len(), 1, "plaintext must be in the vault only");
}

#[test]
fn volatile_capture_crash_is_a_declared_loss_and_leaves_no_trace() {
    let mut buffer = CaptureBuffer::new(4096);
    buffer
        .push(
            CaptureSource::ToolResult,
            b"un-redacted CTXEVAL-SECRET-A1B2C3D4E5",
        )
        .unwrap();
    assert_eq!(buffer.declared_loss(), CaptureLoss::VolatileBytes(39));
    let lost = buffer.simulate_crash();
    assert_eq!(lost, 39);
    assert!(buffer.is_empty());
    assert_eq!(buffer.declared_loss(), CaptureLoss::Empty);
    let mut sink = MemSink::normal();
    let mut txn = IngressTxn::new(4096, 1 << 20);
    txn.commit(&mut sink).unwrap();
    assert!(
        sink.spine.is_empty(),
        "crash must not leave sanitized bytes"
    );
}

#[test]
fn capture_over_capacity_fails_closed() {
    let mut buffer = CaptureBuffer::new(8);
    let err = buffer
        .push(CaptureSource::ToolResult, b"0123456789")
        .unwrap_err();
    assert!(matches!(
        err,
        crate::context_ingress::capture::CaptureError::CapacityExceeded {
            cap: 8,
            requested: 10
        }
    ));
}

#[test]
fn append_before_segmentation_recovery_replays_deterministically() {
    let raw = corpus();
    let mut sink = MemSink::normal();
    let mut txn = IngressTxn::new(1 << 20, 1 << 20);
    txn.capture(CaptureSource::ToolResult, &raw).unwrap();
    let records = txn.commit(&mut sink).unwrap();
    assert!(!sink.spine.is_empty(), "sanitized append must be durable");
    // Crash before item placement: segmentation is re-derived from stored bytes.
    let replayed = segment(&sink.spine);
    assert_eq!(replayed, records[0].segments, "replay must re-derive items");
    assert!(coverage_is_total(&replayed, sink.spine.len()));
}

#[test]
fn generated_artifact_reenters_through_the_same_pipeline() {
    let mut sink = MemSink::normal();
    let mut txn = IngressTxn::new(1 << 20, 1 << 20);
    txn.capture_derivation(
        "fold",
        b"summary: kept the exact error span\nnoise dropped\n",
    )
    .unwrap();
    let records = txn.commit(&mut sink).unwrap();
    assert_eq!(records[0].source, CaptureSource::GeneratedArtifact);
    assert!(coverage_is_total(
        &records[0].segments,
        records[0].sanitized.len()
    ));
}

#[test]
fn generated_summary_cannot_launder_a_quarantined_secret() {
    let mut sink = MemSink::normal();
    let mut txn = IngressTxn::new(1 << 20, 1);
    txn.capture(CaptureSource::ToolResult, &corpus()).unwrap();
    let records = txn.commit(&mut sink).unwrap();
    let quarantine = QuarantineSet::from_records(&records);
    assert!(matches!(
        quarantine.check(b"benign summary"),
        LaunderVerdict::Clean
    ));
    let artifact = format!("summary: {}", "CTXEVAL-SECRET-A1B2C3D4E5");
    assert!(matches!(
        quarantine.check(artifact.as_bytes()),
        LaunderVerdict::Laundered { .. }
    ));
}

#[test]
fn store_read_only_or_unavailable_blocks_ingress_before_side_effects() {
    for mode in ["read-only", "unavailable"] {
        let mut sink = MemSink::normal();
        sink.mode = mode;
        let mut txn = IngressTxn::new(1 << 20, 1 << 20);
        txn.capture(CaptureSource::ToolResult, b"payload").unwrap();
        let err = txn.commit(&mut sink).unwrap_err();
        assert!(
            matches!(err, IngressError::StoreBlocked { .. }),
            "mode {mode}"
        );
    }
}

#[test]
fn segmentation_is_deterministic_and_structurally_classified() {
    let bytes = corpus();
    let first = segment(&bytes);
    let second = segment(&bytes);
    assert_eq!(first, second);
    let classes: Vec<&str> = first.iter().map(|segment| segment.class.name()).collect();
    assert!(classes.contains(&"exact-span"));
    assert!(classes.contains(&"identifier"));
    assert!(classes.contains(&"noise"));
    assert!(first.iter().all(|segment| !segment.class.lane().is_empty()));
}

#[test]
fn filter_digests_exact_ranked_and_noise_with_handles_and_versions() {
    let registry = FilterRegistry::new();
    let bytes = corpus();
    let segments = segment(&bytes);
    let digest = registry.digest(
        "read",
        "raw-handle-1",
        vec![0..bytes.len() as u64],
        &bytes,
        &segments,
    );
    assert_eq!(digest.handle, "raw-handle-1");
    assert_eq!(digest.rule_version, 1);
    assert_eq!(digest.vocabulary_version, 1);
    assert_eq!(digest.class, FilterClass::Exact);
    assert!(
        !digest.preserved.is_empty(),
        "labeled spans must be preserved"
    );
    assert!(String::from_utf8_lossy(&digest.summary).contains("unexpected trailing frame"));
    let noise = b"noise: fill line only 0000\n".to_vec();
    let noise_segments = segment(&noise);
    assert_eq!(
        registry.verdict("read", &noise_segments, noise.len()),
        RuleVerdict::DropBulk
    );
}

#[test]
fn filter_size_floor_passes_verbatim() {
    let registry = FilterRegistry::new();
    let big = vec![b'x'; 2048];
    let segments = segment(&big);
    assert_eq!(
        registry.verdict("read", &segments, big.len()),
        RuleVerdict::PassVerbatim
    );
}

#[test]
fn filter_routes_unusual_unknown_short_spans_verbatim() {
    let registry = FilterRegistry::new();
    let short = b"tiny unknown blob".to_vec();
    let segments = segment(&short);
    assert_eq!(
        registry.verdict("read", &segments, short.len()),
        RuleVerdict::PassVerbatim
    );
}

#[test]
fn filter_rules_are_per_tool_and_version_stable() {
    let mut registry = FilterRegistry::new();
    let mut rules = FilterRules::v1();
    rules.verbatim_tools = vec!["sensitive-tool".to_string()];
    rules.version = 2;
    let applied = registry.update_rules(rules).unwrap();
    assert_eq!(applied, 2);
    let segments = segment(&corpus());
    assert_eq!(
        registry.verdict("sensitive-tool", &segments, corpus().len()),
        RuleVerdict::PassVerbatim
    );
    assert_eq!(
        registry.verdict("other-tool", &segments, corpus().len()),
        RuleVerdict::Digest
    );
    assert_eq!(
        registry.rules_at(1).unwrap().version,
        1,
        "history stays resolvable"
    );
    assert_eq!(registry.rules_at(2).unwrap().version, 2);
}

#[test]
fn filter_relaxation_is_allowed_online_and_tightening_is_rejected() {
    let mut registry = FilterRegistry::new();
    let mut relax = FilterRules::v1();
    relax.version = 2;
    relax.size_floor = 4096;
    assert_eq!(registry.update_rules(relax).unwrap(), 2);
    let mut tighten = FilterRules::v1();
    tighten.version = 3;
    tighten.size_floor = 64;
    let rejected = registry.update_rules(tighten).unwrap_err();
    assert_eq!(rejected.name(), "tightening-requires-offline");
    assert_eq!(
        registry.rules().version,
        2,
        "rejected update must not apply"
    );
    let mut vocab = Vocabulary::v1();
    vocab.version = 2;
    vocab.labels = vec!["error-span", "identifier", "commit-hash"];
    assert_eq!(registry.update_vocabulary(vocab).unwrap(), 2);
    assert_eq!(registry.vocabulary_at(1).unwrap().version, 1);
    let mut shrink = Vocabulary::v1();
    shrink.version = 3;
    shrink.labels = vec!["error-span"];
    assert!(registry.update_vocabulary(shrink).is_err());
}

#[test]
fn filter_preservation_recall_keeps_every_labeled_span() {
    let registry = FilterRegistry::new();
    let bytes = corpus();
    let segments = segment(&bytes);
    let digest: FilterDigest = registry.digest("read", "h", Vec::new(), &bytes, &segments);
    let expected = segments
        .iter()
        .filter(|segment| {
            matches!(
                segment.class,
                StructuralClass::ExactSpan | StructuralClass::Identifier
            )
        })
        .count();
    assert_eq!(digest.preserved.len(), expected);
}

#[test]
fn scan_verdicts_are_distinguished() {
    let detector = Detector::new(
        DetectorClass::CorpusMarker,
        vec!["CTXEVAL-SECRET-"],
        1 << 20,
    );
    assert_eq!(detector.scan(b"nothing here"), ScanVerdict::Clean);
    match detector.scan(b"CTXEVAL-SECRET-xyz tail") {
        ScanVerdict::Detected(found) => assert_eq!(found.len(), 1),
        other => panic!("expected detection, got {other:?}"),
    }
}

#[test]
fn ingress_records_carry_digests_and_exact_spans() {
    let mut sink = MemSink::normal();
    let mut txn = IngressTxn::new(1 << 20, 1 << 20);
    txn.capture(CaptureSource::ToolResult, &corpus()).unwrap();
    let records: Vec<IngressRecord> = txn.commit(&mut sink).unwrap();
    let record = &records[0];
    assert_ne!(record.digest(), 0);
    assert!(!record.exact_spans().is_empty());
}
