use super::*;

#[test]
fn failed_turn_float_telemetry_reopens_retries_and_reads_transcript() {
    let root = tempfile::tempdir().unwrap();
    let session = SessionId::parse("float-roundtrip").unwrap();
    let store = SessionStore::load_at(&session, root.path()).unwrap();
    let reserved = store
        .start_request(None, None, "inspect then finish", root.path())
        .unwrap();
    let telemetry = crate::telemetry::Telemetry {
        cache_hit_ratio: Some(0.12345678901234568),
        rounds: vec![crate::telemetry::ModelCall {
            round: 1,
            attempt: 1,
            gen_ms: Some(1419),
            output_tokens: Some(35),
            tok_s: Some(35.0 * 1000.0 / 1419.0),
            outcome: "ok".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    assert_eq!(
        telemetry.rounds[0].tok_s.unwrap().to_bits(),
        24.665257223396758_f64.to_bits()
    );
    store.record_telemetry(&reserved, &telemetry).unwrap();
    store.fail(&reserved, "server_is_overloaded", &[]).unwrap();
    drop(store);

    let reader = SessionStore::load_at(&session, root.path()).unwrap();
    let state = reader.snapshot().unwrap();
    assert_eq!(state.branches[0].lifecycle, Lifecycle::Failed);
    assert_eq!(
        serde_json::to_vec(state.branches[0].telemetry.as_ref().unwrap()).unwrap(),
        serde_json::to_vec(&telemetry).unwrap()
    );
    let retry = reader
        .start_request(Some(1), None, "inspect then finish", root.path())
        .unwrap();
    assert!(retry.retry);
    reader
        .finalize(
            &retry,
            "recovered",
            &[RoundRecord {
                thinking: String::new(),
                continuation: None,
                assistant: "recovered".into(),
                calls: Vec::new(),
            }],
        )
        .unwrap();
    drop(reader);

    let transcript = SessionStore::read_transcript_at(&session, root.path()).unwrap();
    assert_eq!(transcript.branches.len(), 2);
    assert_eq!(transcript.branches[1].lifecycle, Lifecycle::Completed);
    assert_eq!(transcript.branches[1].summary, "recovered");
    assert_eq!(
        serde_json::to_vec(transcript.branches[0].telemetry.as_ref().unwrap()).unwrap(),
        serde_json::to_vec(&telemetry).unwrap()
    );
}
