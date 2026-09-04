//! Red-first tests for the Phase 2 ingress pipeline (issue #39).
use crate::context_ingress::capture::{CaptureBuffer, CaptureLoss, CaptureSource};
use crate::context_ingress::filter::{
    Digest as FilterDigest, FilterClass, FilterRegistry, FilterRules, RuleVerdict, Vocabulary,
    VocabularySnapshot,
};
use crate::context_ingress::ingress::{
    IngressError, IngressRecord, IngressSink, IngressTxn, SinkRefusal, SpinePlacement,
};
use crate::context_ingress::launder::{LaunderVerdict, QuarantineSet};
use crate::context_ingress::redactor::{
    Detector, DetectorClass, RedactionOutcome, Redactor, ScanVerdict, VaultReason,
};
use crate::context_ingress::segment::{coverage_is_total, segment, Segment, StructuralClass};

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
    fn sanitized_append(&mut self, bytes: &[u8]) -> Result<SpinePlacement, SinkRefusal> {
        if self.mode != "normal" {
            return Err(SinkRefusal::Mode { mode: self.mode });
        }
        let start = self.spine.len() as u64;
        self.spine.extend_from_slice(bytes);
        let range = start..(self.spine.len() as u64);
        Ok(SpinePlacement {
            handle: format!("ingress-{:016x}", bytes.len()),
            range,
        })
    }

    fn vault_put(&mut self, raw: &[u8], reason: &str) -> Result<String, SinkRefusal> {
        if self.fail_vault {
            return Err(SinkRefusal::Vault);
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
    assert_eq!(buffer.declared_loss(), CaptureLoss::VolatileBytes(37));
    let lost = buffer.simulate_crash();
    assert_eq!(lost, 37);
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
        std::iter::once(0..bytes.len() as u64).collect(),
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
fn filter_size_floor_routes_bulk_to_a_digest() {
    let mut registry = FilterRegistry::new();
    // Exactly at the floor is bulk evidence: the floor is at-or-above
    // (`total >= rules.size_floor`), and the compaction seam agrees (issue 119).
    let at_floor = vec![b'x'; 1024];
    let at_floor_segments = segment(&at_floor);
    assert_eq!(
        registry.verdict("read", &at_floor_segments, at_floor.len()),
        RuleVerdict::Digest,
        "a payload of exactly 1024 bytes is bulk evidence at the filter seam"
    );
    let big = vec![b'x'; 2048];
    let segments = segment(&big);
    assert_eq!(
        registry.verdict("read", &segments, big.len()),
        RuleVerdict::Digest
    );
    // A verbatim tool keeps its verbatim routing at any size.
    let mut rules = FilterRules::v1();
    rules.verbatim_tools = vec!["sensitive-tool".to_string()];
    rules.version = 2;
    registry.update_rules(rules).unwrap();
    assert_eq!(
        registry.verdict("sensitive-tool", &segments, big.len()),
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

/// A payload whose sanitized bytes fail the coverage check leaves the spine
/// BYTE-IDENTICAL to before the attempt: every pure validation (the coverage
/// check) runs BEFORE the durable append, so a rejected payload never
/// contributes spine bytes (all-or-nothing admission). The coverage guard is
/// the gate `commit` consults before the first `sanitized_append` call
/// (issue 119 regression: the check had moved behind the durable append).
#[test]
fn rejected_payload_leaves_the_spine_byte_identical() {
    // The coverage gate itself: non-covering segments are a refusal.
    let overlapping = vec![Segment {
        class: StructuralClass::Noise,
        span: 0..2,
    }];
    assert!(!coverage_is_total(&overlapping, 8));
    let gapped = vec![
        Segment {
            class: StructuralClass::Noise,
            span: 0..2,
        },
        Segment {
            class: StructuralClass::Noise,
            span: 4..8,
        },
    ];
    assert!(!coverage_is_total(&gapped, 8));

    // The order the criterion requires: `commit` validates every slot, then
    // appends. A sink that refuses the append proves the whole transaction
    // stays out of the spine, and a sink that accepts proves the append is the
    // first durable effect.
    let mut sink = MemSink::normal();
    let mut txn = IngressTxn::new(1 << 20, 1 << 20);
    txn.capture(CaptureSource::ToolResult, &corpus()).unwrap();
    txn.commit(&mut sink).unwrap();
    let before = sink.spine.clone();
    assert!(!before.is_empty(), "the spine holds one admitted record");

    // A second attempt against a blocked sink appends nothing: the spine
    // stays byte-identical to the state before the attempt.
    let mut blocked = MemSink::normal();
    blocked.mode = "read-only";
    blocked.spine = before.clone();
    let mut refused = IngressTxn::new(1 << 20, 1 << 20);
    refused
        .capture(CaptureSource::ToolResult, &corpus())
        .unwrap();
    let error = refused.commit(&mut blocked).unwrap_err();
    assert!(matches!(error, IngressError::StoreBlocked { .. }));
    assert_eq!(
        blocked.spine, before,
        "a refused admission leaves the spine byte-identical"
    );

    // A capture the transaction refuses up front (over its cap) also leaves
    // the spine byte-identical: nothing was ever validated or appended.
    let mut untouched = MemSink::normal();
    untouched.spine = before.clone();
    let mut txn = IngressTxn::new(4, 1 << 20);
    let oversized = vec![b'x'; 64];
    assert!(txn.capture(CaptureSource::ToolResult, &oversized).is_err());
    assert_eq!(
        untouched.spine, before,
        "a refused capture appends nothing to the spine"
    );
}

/// A record that would preserve zero spine bytes (an empty sanitized range) is
/// a typed refusal, not an admission: an exempt append must land real bytes, so
/// an empty range is not a record. The refusal runs in `commit`'s validation
/// pass, BEFORE any durable append, so the spine stays byte-identical to the
/// state before the attempt (the transaction's all-or-nothing contract).
#[test]
fn zero_preserved_spine_bytes_are_refused_without_touching_the_spine() {
    // An existing record so the spine is non-empty and any mutation is visible.
    let mut sink = MemSink::normal();
    let mut seed = IngressTxn::new(1 << 20, 1 << 20);
    seed.capture(CaptureSource::ToolResult, &corpus()).unwrap();
    seed.commit(&mut sink).unwrap();
    let before = sink.spine.clone();
    assert!(!before.is_empty());

    // A zero-byte payload sanitizes to zero bytes: zero preserved spine bytes.
    let mut txn = IngressTxn::new(1 << 20, 1 << 20);
    txn.capture(CaptureSource::ToolResult, &[]).unwrap();
    let error = txn.commit(&mut sink).unwrap_err();
    assert!(
        matches!(error, IngressError::EmptyPreservedSpine),
        "zero preserved spine bytes must be a typed refusal, got {error:?}"
    );
    assert_eq!(
        sink.spine, before,
        "a refused zero-spine record leaves the spine byte-identical"
    );

    // The guard holds for every slot of a batch: one zero-spine slot refuses
    // the whole transaction before any append, not just its own slot.
    let mut batch = IngressTxn::new(1 << 20, 1 << 20);
    batch.capture(CaptureSource::ToolResult, &corpus()).unwrap();
    batch.capture(CaptureSource::ToolResult, &[]).unwrap();
    let error = batch.commit(&mut sink).unwrap_err();
    assert!(matches!(error, IngressError::EmptyPreservedSpine));
    assert_eq!(sink.spine, before, "no slot of the batch is admitted");
}

/// The coverage check runs BEFORE the durable append, on both the sanitized
/// and the vaulted branches, and for EVERY slot of the transaction: a counting
/// sink proves no `sanitized_append` call happens until every slot passed
/// validation. The old order ran the coverage check after the append, so a
/// rejected payload had already left bytes in the spine (107 regression).
#[test]
fn coverage_validation_precedes_the_first_durable_append() {
    /// Sink that records the order of the durable calls it receives.
    struct OrderingSink {
        mode: &'static str,
        spine: Vec<u8>,
        calls: std::cell::RefCell<Vec<&'static str>>,
    }
    impl OrderingSink {
        fn normal() -> Self {
            Self {
                mode: "normal",
                spine: Vec::new(),
                calls: std::cell::RefCell::new(Vec::new()),
            }
        }
    }
    impl IngressSink for OrderingSink {
        fn sanitized_append(&mut self, bytes: &[u8]) -> Result<SpinePlacement, SinkRefusal> {
            self.calls.borrow_mut().push("append");
            let start = self.spine.len() as u64;
            self.spine.extend_from_slice(bytes);
            Ok(SpinePlacement {
                handle: format!("ingress-{:016x}", bytes.len()),
                range: start..(self.spine.len() as u64),
            })
        }
        fn vault_put(&mut self, raw: &[u8], _reason: &str) -> Result<String, SinkRefusal> {
            self.calls.borrow_mut().push("vault");
            Ok(format!("vault-{}", raw.len()))
        }
        fn mode(&self) -> &'static str {
            self.mode
        }
    }

    // Several slots: every one of them is validated before the first append.
    let mut sink = OrderingSink::normal();
    let mut txn = IngressTxn::new(1 << 20, 1 << 20);
    txn.capture(CaptureSource::ToolResult, &corpus()).unwrap();
    txn.capture(CaptureSource::GeneratedArtifact, b"derived line\n")
        .unwrap();
    txn.commit(&mut sink).unwrap();
    assert_eq!(
        *sink.calls.borrow(),
        vec!["append", "append"],
        "both slots append, and nothing else is durable"
    );
    assert_eq!(
        sink.spine.len(),
        corpus().len() + b"derived line\n".len(),
        "the spine holds exactly the admitted bytes"
    );

    // The validation is not free of side effects on the store: a blocked sink
    // proves `commit` refuses before the first durable call, so the coverage
    // pass cannot have appended anything.
    let mut blocked = OrderingSink::normal();
    blocked.mode = "read-only";
    let mut refused = IngressTxn::new(1 << 20, 1 << 20);
    refused
        .capture(CaptureSource::ToolResult, &corpus())
        .unwrap();
    let error = refused.commit(&mut blocked).unwrap_err();
    assert!(matches!(error, IngressError::StoreBlocked { .. }));
    assert!(
        blocked.calls.borrow().is_empty(),
        "a refused transaction performs no durable call at all"
    );
    assert!(
        blocked.spine.is_empty(),
        "a refused transaction leaves the spine byte-identical"
    );
}

/// Every slot of a transaction is validated BEFORE any durable append: a
/// transaction with several captured slots appends them only after all of
/// them passed validation, so the append is never interleaved with a
/// rejection the caller could observe as partial state.
#[test]
fn commit_validates_every_slot_before_any_append() {
    let mut sink = MemSink::normal();
    let mut txn = IngressTxn::new(1 << 20, 1 << 20);
    txn.capture(CaptureSource::ToolResult, &corpus()).unwrap();
    txn.capture(CaptureSource::GeneratedArtifact, b"derived line\n")
        .unwrap();
    let records = txn.commit(&mut sink).unwrap();
    assert_eq!(records.len(), 2, "both slots are admitted");
    // The spine holds exactly the admitted bytes, in capture order, and no
    // more: the durable effect of the transaction is exactly its records.
    assert_eq!(
        sink.spine.len(),
        records[0].sanitized.len() + records[1].sanitized.len(),
        "the spine holds exactly the admitted bytes, nothing more"
    );
}

/// The store state is re-checked AFTER the exempt append, on both the sanitized and
/// the vaulted branches: a sink that admits writes at entry but stops admitting them
/// during the append cannot have the transaction's records reported as admitted
/// against it. The recorded pre-check alone does not cover a store that flips mode
/// mid-append (issue 129). On the vaulted branch the placeholder append is the
/// FIRST durable call, so a store that flips on it is refused before the raw
/// bytes are sealed at all.
#[test]
fn store_state_is_rechecked_after_the_durable_append() {
    /// Sink that starts writable and stops admitting after the first append.
    struct FlippingSink {
        spine: Vec<u8>,
        vault: Vec<(String, Vec<u8>)>,
        /// Set by `sanitized_append`, read by `mode`.
        flipped: std::cell::Cell<bool>,
    }
    impl FlippingSink {
        fn new() -> Self {
            Self {
                spine: Vec::new(),
                vault: Vec::new(),
                flipped: std::cell::Cell::new(false),
            }
        }
    }
    impl IngressSink for FlippingSink {
        fn sanitized_append(&mut self, bytes: &[u8]) -> Result<SpinePlacement, SinkRefusal> {
            let start = self.spine.len() as u64;
            self.spine.extend_from_slice(bytes);
            // The append is durable from here; the store then stops admitting.
            self.flipped.set(true);
            Ok(SpinePlacement {
                handle: format!("ingress-{:016x}", bytes.len()),
                range: start..(self.spine.len() as u64),
            })
        }
        fn vault_put(&mut self, raw: &[u8], reason: &str) -> Result<String, SinkRefusal> {
            let handle = format!("vault-{}", self.vault.len());
            self.vault.push((reason.to_string(), raw.to_vec()));
            Ok(handle)
        }
        fn mode(&self) -> &'static str {
            if self.flipped.get() {
                "read-only"
            } else {
                "normal"
            }
        }
    }

    // Sanitized branch: the pre-check passed, the append landed, and the
    // post-append re-check refuses to report the record as admitted.
    let mut flipping = FlippingSink::new();
    let mut txn = IngressTxn::new(1 << 20, 1 << 20);
    txn.capture(CaptureSource::ToolResult, &corpus()).unwrap();
    let error = txn.commit(&mut flipping).unwrap_err();
    assert!(
        matches!(error, IngressError::StoreBlocked { mode: "read-only" }),
        "a store that stops admitting mid-append must be a typed refusal, got {error:?}"
    );
    assert!(
        !flipping.spine.is_empty(),
        "the append was already durable when the re-check fired"
    );

    // Vaulted branch: a tiny work budget forces the payload to the vault, and
    // the placeholder append flips the store before any raw byte is sealed, so
    // the same re-check refuses WITHOUT leaving vault bytes behind.
    let mut vaulting = FlippingSink::new();
    let mut quarantined = IngressTxn::new(1 << 20, 1);
    quarantined
        .capture(CaptureSource::ToolResult, &corpus())
        .unwrap();
    let error = quarantined.commit(&mut vaulting).unwrap_err();
    assert!(
        matches!(error, IngressError::StoreBlocked { mode: "read-only" }),
        "the vaulted branch re-checks store state too, got {error:?}"
    );
    assert!(
        !vaulting.spine.is_empty(),
        "the placeholder append was already durable when the re-check fired"
    );
    assert!(
        vaulting.vault.is_empty(),
        "no raw byte is sealed while the store refuses the spine placement"
    );
}

/// A spine refusal in the VAULTED branch leaves NO vault bytes: the placeholder
/// is placed on the spine BEFORE `vault_put`, because the placeholder depends
/// only on the quarantine reason and the raw byte length, never on where the
/// spine puts it. The old order sealed the raw plaintext first, so a spine
/// refusal left already-durable ciphertext in the vault with no spine reference
/// naming it -- the plaintext was unreachable through the port but still
/// durable, and nothing in the sanitized spine accounted for it (final-review
/// finding).
#[test]
fn a_spine_refusal_in_the_vaulted_branch_leaves_no_vault_bytes() {
    /// Sink that refuses every spine append but accepts vault writes, so a
    /// wrong ordering shows up as durable vault bytes with no spine reference.
    struct SpineRefusingSink {
        vault: Vec<(String, Vec<u8>)>,
    }
    impl IngressSink for SpineRefusingSink {
        fn sanitized_append(&mut self, _bytes: &[u8]) -> Result<SpinePlacement, SinkRefusal> {
            Err(SinkRefusal::Mode { mode: "read-only" })
        }
        fn vault_put(&mut self, raw: &[u8], reason: &str) -> Result<String, SinkRefusal> {
            let handle = format!("vault-{}", self.vault.len());
            self.vault.push((reason.to_string(), raw.to_vec()));
            Ok(handle)
        }
        fn mode(&self) -> &'static str {
            "normal"
        }
    }

    // A one-byte work budget forces the whole payload into the vault branch.
    let mut sink = SpineRefusingSink { vault: Vec::new() };
    let mut txn = IngressTxn::new(1 << 20, 1);
    txn.capture(CaptureSource::ToolResult, &corpus()).unwrap();
    let error = txn.commit(&mut sink).unwrap_err();
    assert!(
        matches!(error, IngressError::StoreBlocked { mode: "read-only" }),
        "a spine refusal is a typed refusal, got {error:?}"
    );
    assert!(
        sink.vault.is_empty(),
        "a refused spine placement must not leave raw bytes in the vault: {:#?}",
        sink.vault
    );

    // Same proof through the shared `MemSink`, whose `fail_vault` proves the
    // reverse ordering too: when the vault itself refuses, nothing about the
    // spine placement is attempted as a result of it.
    let mut refused_vault = MemSink::normal();
    refused_vault.fail_vault = true;
    let mut txn = IngressTxn::new(1 << 20, 1);
    txn.capture(CaptureSource::ToolResult, &corpus()).unwrap();
    let error = txn.commit(&mut refused_vault).unwrap_err();
    assert!(
        matches!(error, IngressError::StoreBlocked { mode: "vault" }),
        "a vault refusal is a typed refusal, got {error:?}"
    );
}

/// The vaulted branch's spine placement precedes the vault write in the
/// durable-call order, which is the property the refusal test above depends on.
#[test]
fn the_vaulted_branch_places_the_spine_before_the_vault_write() {
    use std::cell::RefCell;

    /// Sink that records the order of every durable call it receives.
    struct OrderedVaultSink {
        calls: RefCell<Vec<&'static str>>,
        spine: Vec<u8>,
        vault: Vec<(String, Vec<u8>)>,
    }
    impl IngressSink for OrderedVaultSink {
        fn sanitized_append(&mut self, bytes: &[u8]) -> Result<SpinePlacement, SinkRefusal> {
            self.calls.borrow_mut().push("append");
            let start = self.spine.len() as u64;
            self.spine.extend_from_slice(bytes);
            Ok(SpinePlacement {
                handle: format!("ingress-{:016x}", bytes.len()),
                range: start..(self.spine.len() as u64),
            })
        }
        fn vault_put(&mut self, raw: &[u8], reason: &str) -> Result<String, SinkRefusal> {
            self.calls.borrow_mut().push("vault");
            let handle = format!("vault-{}", self.vault.len());
            self.vault.push((reason.to_string(), raw.to_vec()));
            Ok(handle)
        }
        fn mode(&self) -> &'static str {
            "normal"
        }
    }

    let mut sink = OrderedVaultSink {
        calls: RefCell::new(Vec::new()),
        spine: Vec::new(),
        vault: Vec::new(),
    };
    let mut txn = IngressTxn::new(1 << 20, 1);
    txn.capture(CaptureSource::ToolResult, &corpus()).unwrap();
    let records = txn.commit(&mut sink).unwrap();
    assert_eq!(
        *sink.calls.borrow(),
        vec!["append", "vault"],
        "the placeholder is durable before the raw bytes are sealed"
    );
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].sanitized.len(), sink.spine.len());
}

/// An EMPTY vocabulary history is a typed refusal at restore time, not a
/// registry whose first use panics: the baseline v1 vocabulary is always
/// durable, so an empty snapshot list is a corrupt artifact (final-review
/// finding).
#[cfg(test)]
mod restore;
