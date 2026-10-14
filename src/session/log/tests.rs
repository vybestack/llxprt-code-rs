use super::*;

fn branch() -> BranchRecord {
    BranchRecord {
        telemetry: None,
        branch_id: "b1".into(),
        turn: 1,
        attempt: 1,
        parent_branch: None,
        parent_turn: 0,
        parent_attempt: 0,
        prompt: "hello".into(),
        digest: crate::limits::prompt_digest("hello"),
        lifecycle: Lifecycle::Pending,
        rounds: Vec::new(),
        stream_retries: Vec::new(),
        summary: String::new(),
        error: String::new(),
        owner: "owner".into(),
        reserved_at: 1,
        lease_expiry: 2,
    }
}

fn reservation() -> Event {
    Event::BranchReserved {
        cwd: Some("/workspace".into()),
        cwd_dev: 1,
        cwd_ino: 1,
        branch: Box::new(branch()),
        next_branch_seq: 1,
    }
}

fn cursor() -> ReplayCursor {
    ReplayCursor {
        seq: 0,
        offset: 0,
        digest: [0; DIGEST_LEN],
        events: 0,
    }
}

fn replay(bytes: &[u8], repair: bool) -> Result<(SessionState, ReplayResult), StoreError> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("segment.log");
    std::fs::write(&path, bytes).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    let mut state = SessionState::empty("test");
    let result = replay_from(&mut file, &mut state, cursor(), repair)?;
    Ok((state, result))
}

#[test]
fn every_incomplete_frame_prefix_is_repaired_without_partial_transaction() {
    let frame = encode_frame(1, [0; DIGEST_LEN], &[reservation()]).unwrap();
    for length in 0..frame.bytes.len() {
        let (state, result) = replay(&frame.bytes[..length], true)
            .unwrap_or_else(|error| panic!("prefix {length}: {error}"));
        assert!(state.branches.is_empty(), "prefix {length} published state");
        assert_eq!(result.cursor.seq, 0);
        assert_eq!(result.repaired_tail, length != 0);
    }
    let (state, result) = replay(&frame.bytes, true).unwrap();
    assert_eq!(state.branches.len(), 1);
    assert_eq!(result.cursor.seq, 1);
}

#[test]
fn complete_bad_digest_and_trailing_garbage_are_corrupt() {
    let frame = encode_frame(1, [0; DIGEST_LEN], &[reservation()]).unwrap();
    for position in [0, HEADER_LEN, frame.bytes.len() - 1] {
        let mut bad = frame.bytes.clone();
        bad[position] ^= 1;
        assert!(matches!(replay(&bad, true), Err(StoreError::Corrupt(_))));
    }
    let mut garbage = frame.bytes;
    garbage.extend_from_slice(b"garbage");
    assert!(matches!(
        replay(&garbage, true),
        Err(StoreError::Corrupt(_))
    ));
}

#[test]
fn sequence_gap_and_digest_chain_break_are_corrupt() {
    let gap = encode_frame(2, [0; DIGEST_LEN], &[reservation()]).unwrap();
    assert!(matches!(
        replay(&gap.bytes, true),
        Err(StoreError::Corrupt(_))
    ));
    let chained = encode_frame(1, [9; DIGEST_LEN], &[reservation()]).unwrap();
    assert!(matches!(
        replay(&chained.bytes, true),
        Err(StoreError::Corrupt(_))
    ));
}

fn telemetry_event(tok_s: f64, cache_hit_ratio: f64) -> Event {
    Event::Telemetry {
        branch_id: "b1".into(),
        owner: "owner".into(),
        telemetry: Box::new(crate::telemetry::Telemetry {
            cache_hit_ratio: Some(cache_hit_ratio),
            rounds: vec![crate::telemetry::ModelCall {
                round: 1,
                attempt: 1,
                tok_s: Some(tok_s),
                ..Default::default()
            }],
            ..Default::default()
        }),
    }
}

#[test]
fn telemetry_float_roundtrip_replays_exact_written_state() {
    let reserved = encode_frame(1, [0; DIGEST_LEN], &[reservation()]).unwrap();
    let telemetry = telemetry_event(24.665257223396758, 0.12345678901234568);
    let frame = encode_frame(2, reserved.digest, &[telemetry]).unwrap();
    let mut expected = SessionState::empty("test");
    super::super::replay::apply_batch(&mut expected, &reserved.batch).unwrap();
    super::super::replay::apply_batch(&mut expected, &frame.batch).unwrap();
    let bytes = [reserved.bytes, frame.bytes].concat();
    let (recovered, result) = replay(&bytes, false).unwrap();
    assert_eq!(result.cursor.seq, 2);
    assert_eq!(
        serde_json::to_vec(&recovered).unwrap(),
        serde_json::to_vec(&expected).unwrap()
    );
}

#[test]
fn telemetry_float_roundtrip_covers_derived_rates_and_cache_ratios() {
    // Deterministic property-style coverage of both persisted f64 fields, including
    // rates computed with the production tokens * 1000 / milliseconds formula.
    for n in 1..=4096_u64 {
        let tokens = n * 7919;
        let ms = n * 17 + 1;
        let event = telemetry_event(
            tokens as f64 * 1000.0 / ms as f64,
            n as f64 / (n + 37) as f64,
        );
        let frame = encode_frame(1, [0; DIGEST_LEN], &[event]).unwrap();
        let payload_len = u32_at(&frame.bytes, 12) as usize;
        let decoded = decode_batch(&frame.bytes, payload_len, &frame.bytes[..HEADER_LEN])
            .unwrap_or_else(|error| panic!("sample {n}: {error}"));
        assert_eq!(
            serde_json::to_vec(&decoded.events).unwrap(),
            serde_json::to_vec(&frame.batch.events).unwrap(),
            "sample {n}"
        );
    }
}

fn fixture_frame(payload: &[u8], previous: [u8; DIGEST_LEN]) -> Vec<u8> {
    // Construct a frame from fixed pre-fix payload bytes, never from encode_frame
    // or a parse/re-serialize. Its txn_id hashes the original events bytes.
    let batch: EventBatch = serde_json::from_slice(payload).unwrap();
    let mut bytes = MAGIC.to_vec();
    bytes.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&2_u64.to_le_bytes());
    bytes.extend_from_slice(&Sha256::digest(batch.txn_id.as_bytes())[..16]);
    bytes.extend_from_slice(&previous);
    bytes.extend_from_slice(payload);
    let digest = digest_prefix(&bytes);
    bytes.extend_from_slice(&digest);
    bytes
}

#[test]
fn telemetry_float_roundtrip_reads_existing_payload_without_rewriting() {
    let reserved = encode_frame(1, [0; DIGEST_LEN], &[reservation()]).unwrap();
    let payload = include_bytes!("fixtures/telemetry-float.json");
    let bytes = [reserved.bytes, fixture_frame(payload, reserved.digest)].concat();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("segment.log");
    std::fs::write(&path, &bytes).unwrap();
    let mut file = std::fs::File::open(&path).unwrap();
    let mut state = SessionState::empty("test");
    let result = replay_read_mode(&mut file, &mut state, cursor(), false, false).unwrap();
    let telemetry = state.branches[0].telemetry.as_ref().unwrap();
    assert_eq!(
        telemetry.rounds[0].tok_s.unwrap().to_bits(),
        24.665257223396758_f64.to_bits()
    );
    assert_eq!(
        telemetry.cache_hit_ratio.unwrap().to_bits(),
        0.12345678901234568_f64.to_bits()
    );
    assert_eq!(result.cursor.seq, 2);
    assert!(!result.repaired_tail);
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[test]
fn telemetry_float_roundtrip_still_rejects_wrong_transaction_id() {
    let reserved = encode_frame(1, [0; DIGEST_LEN], &[reservation()]).unwrap();
    let payload = std::str::from_utf8(include_bytes!("fixtures/telemetry-float.json")).unwrap();
    // Update a float and both frame checksums, but not the transaction id.
    let bad = payload.replace("24.665257223396758", "24.66525722339676");
    let bytes = [
        reserved.bytes,
        fixture_frame(bad.as_bytes(), reserved.digest),
    ]
    .concat();
    let error = replay(&bytes, false)
        .err()
        .expect("reject wrong transaction ID");
    assert!(matches!(error, StoreError::Corrupt(_)));
    assert_eq!(
        error.to_string(),
        "corrupt session state: session transaction id mismatch"
    );
}
