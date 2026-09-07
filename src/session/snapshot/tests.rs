use super::*;

fn open(path: &std::path::Path) -> openat::Dir {
    openat::Dir::open(path).expect("open test session directory")
}

fn reservation_event(prompt: &str) -> log::Event {
    log::Event::BranchReserved {
        cwd: Some("/workspace".into()),
        cwd_dev: 1,
        cwd_ino: 1,
        branch: BranchRecord {
            branch_id: "b1".into(),
            turn: 1,
            attempt: 1,
            parent_branch: None,
            parent_turn: 0,
            parent_attempt: 0,
            prompt: prompt.into(),
            digest: crate::limits::prompt_digest(prompt),
            lifecycle: Lifecycle::Pending,
            rounds: Vec::new(),
            summary: String::new(),
            error: String::new(),
            owner: "owner".into(),
            reserved_at: 1,
            lease_expiry: 2,
        },
        next_branch_seq: 1,
    }
}

#[test]
fn current_manifest_and_log_load_the_store_they_describe() {
    let root = tempfile::tempdir().unwrap();
    let dir = open(root.path());
    let state = SessionState::empty("current-store");
    let manifest = initial_manifest(&dir, &state, 0, [0; 16], None).unwrap();
    let mut loaded = load(&dir).unwrap();
    append(&dir, &mut loaded, vec![reservation_event("committed")]).unwrap();

    let reopened = load(&dir).unwrap();
    assert_eq!(reopened.state.session_id, "current-store");
    assert_eq!(reopened.state.branches[0].prompt, "committed");
    assert_eq!(
        std::fs::read(root.path().join("session.manifest.json")).unwrap(),
        serde_json::to_vec(&manifest).unwrap(),
        "appending changes only the manifest-selected segment"
    );
}

#[test]
fn missing_manifest_rejects_unrecognized_files_without_mutation() {
    let root = tempfile::tempdir().unwrap();
    let session = SessionId::parse("old").unwrap();
    let path = root.path().join("code-rs-sessions").join(&session.id);
    std::fs::create_dir_all(&path).unwrap();
    let primary = path.join("session.json");
    let alternate = path.join("session.alt.json");
    std::fs::write(
        &primary,
        serde_json::to_vec(&SessionState::empty("old")).unwrap(),
    )
    .unwrap();
    std::fs::write(
        &alternate,
        br#"{"store_generation":7,"state":{"version":2,"session_id":"old","cwd":null,"cwd_dev":0,"cwd_ino":0,"branches":[],"next_branch_seq":0}}"#,
    )
    .unwrap();
    let before = [
        std::fs::read(&primary).unwrap(),
        std::fs::read(&alternate).unwrap(),
    ];

    let error = match SessionStore::load_at(&session, root.path()) {
        Ok(_) => panic!("unrecognized directory unexpectedly loaded"),
        Err(error) => error,
    };
    assert!(matches!(error, StoreError::Corrupt(_)));
    assert_eq!(
        error.to_string(),
        "corrupt session state: session manifest is missing"
    );
    assert_eq!(
        [
            std::fs::read(&primary).unwrap(),
            std::fs::read(&alternate).unwrap()
        ],
        before,
        "rejection neither converts nor removes unrecognized files"
    );
    assert_eq!(
        {
            let mut names = std::fs::read_dir(&path)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>();
            names.sort();
            names
        },
        vec![
            std::ffi::OsString::from("session.alt.json"),
            std::ffi::OsString::from("session.json")
        ],
        "rejection creates no store artifacts"
    );
}

#[test]
fn malformed_current_manifest_is_corrupt() {
    let root = tempfile::tempdir().unwrap();
    let dir = open(root.path());
    std::fs::write(root.path().join("session.manifest.json"), b"not json").unwrap();

    assert!(matches!(load(&dir), Err(StoreError::Corrupt(_))));
}

#[test]
fn malformed_current_log_is_corrupt() {
    let root = tempfile::tempdir().unwrap();
    let dir = open(root.path());
    let state = SessionState::empty("bad-log");
    let manifest = initial_manifest(&dir, &state, 0, [0; 16], None).unwrap();
    std::fs::write(root.path().join(manifest.current.segment), vec![b'x'; 56]).unwrap();

    assert!(matches!(load(&dir), Err(StoreError::Corrupt(_))));
}

#[test]
fn oversize_current_segment_recovers_retained_previous_set() {
    let root = tempfile::tempdir().unwrap();
    let dir = open(root.path());
    let base = SessionState::empty("fallback");
    let first = initial_manifest(&dir, &base, 0, [0; 16], None).unwrap();
    let mut loaded = load_set(&dir, &first, &first.current, true).unwrap();
    append(&dir, &mut loaded, vec![reservation_event("committed")]).unwrap();
    compact(&dir, &mut loaded).unwrap();

    let current_segment = root.path().join(&loaded.manifest.current.segment);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&current_segment)
        .unwrap()
        .set_len(log::max_replay_bytes() + 1)
        .unwrap();

    let recovered = load(&dir).unwrap();
    assert_eq!(recovered.state.branches.len(), 1);
    assert_eq!(recovered.state.branches[0].prompt, "committed");
}

#[test]
fn compaction_error_is_returned_after_committed_append() {
    let root = tempfile::tempdir().unwrap();
    let dir = open(root.path());
    let state = SessionState::empty("compact-error");
    let manifest = initial_manifest(&dir, &state, 0, [0; 16], None).unwrap();
    let mut loaded = load_set(&dir, &manifest, &manifest.current, true).unwrap();
    loaded.cursor.events = log::EVENT_THRESHOLD - 1;

    std::fs::create_dir(root.path().join("snapshot-1-1.json")).unwrap();
    let result = append(&dir, &mut loaded, vec![reservation_event("committed")]);

    assert!(matches!(result, Err(StoreError::CommittedMaintenance(_))));
    std::fs::remove_dir(root.path().join("snapshot-1-1.json")).unwrap();
    let reopened = load(&dir).unwrap();
    assert_eq!(reopened.state.branches.len(), 1);
}

#[test]
fn reclaim_event_replaces_prompt_and_digest() {
    let root = tempfile::tempdir().unwrap();
    let dir = open(root.path());
    let mut state = SessionState::empty("reclaim-prompt");
    replay::apply_batch(
        &mut state,
        &log::EventBatch {
            txn_id: "setup".into(),
            events: vec![reservation_event("old prompt")],
        },
    )
    .unwrap();
    let manifest = initial_manifest(&dir, &state, 0, [0; 16], None).unwrap();
    let mut loaded = load_set(&dir, &manifest, &manifest.current, true).unwrap();

    append(
        &dir,
        &mut loaded,
        vec![log::Event::BranchReclaimed {
            branch_id: "b1".into(),
            prompt: "different prompt".into(),
            owner: "new-owner".into(),
            reserved_at: 2,
            lease_expiry: 3,
        }],
    )
    .unwrap();

    let branch = &loaded.state.branches[0];
    assert_eq!(branch.prompt, "different prompt");
    assert_eq!(
        branch.digest,
        crate::limits::prompt_digest("different prompt")
    );
    assert_eq!(branch.owner, "new-owner");
}

#[test]
fn replaced_snapshot_retains_a_loadable_previous_set() {
    let root = tempfile::tempdir().unwrap();
    let dir = open(root.path());
    let state = SessionState::empty("replace-fallback");
    let manifest = initial_manifest(&dir, &state, 0, [0; 16], None).unwrap();
    let mut loaded = load_set(&dir, &manifest, &manifest.current, true).unwrap();
    append(&dir, &mut loaded, vec![reservation_event("retained")]).unwrap();

    let loaded = replace_materialized(&dir, &loaded.state).unwrap();
    std::fs::write(
        root.path().join(&loaded.manifest.current.snapshot),
        b"corrupt current snapshot",
    )
    .unwrap();

    let recovered = load(&dir).unwrap();
    assert_eq!(recovered.state.branches.len(), 1);
    assert_eq!(recovered.state.branches[0].prompt, "retained");
}

#[test]
fn failed_catch_up_does_not_partially_mutate_cached_state() {
    let root = tempfile::tempdir().unwrap();
    let dir = open(root.path());
    let state = SessionState::empty("atomic-catch-up");
    let manifest = initial_manifest(&dir, &state, 0, [0; 16], None).unwrap();
    let mut cached = load_set(&dir, &manifest, &manifest.current, true).unwrap();
    let mut writer = load_set(&dir, &manifest, &manifest.current, true).unwrap();
    append(&dir, &mut writer, vec![reservation_event("once")]).unwrap();
    let committed_len = writer.cursor.offset;
    let segment = root.path().join(&manifest.current.segment);
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&segment)
        .unwrap();
    std::io::Write::write_all(&mut file, b"garbage").unwrap();
    file.sync_all().unwrap();

    assert!(matches!(
        catch_up(&dir, &mut cached),
        Err(StoreError::Corrupt(_))
    ));
    assert!(cached.state.branches.is_empty());
    std::fs::OpenOptions::new()
        .write(true)
        .open(segment)
        .unwrap()
        .set_len(committed_len)
        .unwrap();
    catch_up(&dir, &mut cached).unwrap();
    assert_eq!(cached.state.branches.len(), 1);
    assert_eq!(cached.state.branches[0].prompt, "once");
}

#[test]
fn dual_recovery_failure_preserves_causes_and_io_semantics() {
    let root = tempfile::tempdir().unwrap();
    let dir = open(root.path());
    let state = SessionState::empty("dual-failure");
    let manifest = initial_manifest(&dir, &state, 0, [0; 16], None).unwrap();
    let mut loaded = load_set(&dir, &manifest, &manifest.current, true).unwrap();
    append(&dir, &mut loaded, vec![reservation_event("retained")]).unwrap();
    compact(&dir, &mut loaded).unwrap();

    let current_snapshot = root.path().join(&loaded.manifest.current.snapshot);
    std::fs::remove_file(&current_snapshot).unwrap();
    std::fs::create_dir(&current_snapshot).unwrap();
    let previous_segment = root
        .path()
        .join(&loaded.manifest.previous.as_ref().unwrap().segment);
    std::fs::OpenOptions::new()
        .append(true)
        .open(previous_segment)
        .unwrap()
        .write_all(b"corrupt retained segment")
        .unwrap();

    let error = match load(&dir) {
        Ok(_) => panic!("dual recovery failure unexpectedly loaded"),
        Err(error) => error,
    };
    assert!(matches!(error, StoreError::Io(_)));
    let message = error.to_string();
    assert!(message.contains("current recovery set failed"));
    assert!(message.contains("retained recovery set failed"));
}
