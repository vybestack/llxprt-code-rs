use super::*;

fn branch() -> BranchRecord {
    BranchRecord {
        branch_id: "b1".into(),
        turn: 1,
        attempt: 1,
        parent_branch: None,
        parent_turn: 0,
        parent_attempt: 0,
        prompt: "hello".into(),
        digest: crate::agent::prompt_digest("hello"),
        lifecycle: Lifecycle::Pending,
        rounds: Vec::new(),
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
        branch: branch(),
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
