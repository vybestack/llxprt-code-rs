//! Current-format reader selection, lineage and no-write guarantees.
use llxprt_code_rs::session::{RoundRecord, SessionId, SessionStore};
use std::path::{Path, PathBuf};

fn files(path: &Path) -> Vec<(PathBuf, Vec<u8>, std::time::SystemTime)> {
    let mut result = Vec::new();
    for entry in std::fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            result.extend(files(&path));
        } else {
            result.push((
                path.clone(),
                std::fs::read(&path).unwrap(),
                std::fs::metadata(path).unwrap().modified().unwrap(),
            ));
        }
    }
    result.sort();
    result
}

fn complete(
    store: &SessionStore,
    turn: Option<u32>,
    parent: Option<&str>,
    prompt: &str,
    root: &Path,
) -> String {
    let request = store.start_request(turn, parent, prompt, root).unwrap();
    store
        .finalize(
            &request,
            prompt,
            &[RoundRecord {
                assistant: prompt.into(),
                calls: vec![],
            }],
        )
        .unwrap();
    request.branch_id
}

#[test]
fn read_preserves_all_lineages_and_can_run_concurrently() {
    let temp = tempfile::tempdir().unwrap();
    let id = SessionId::parse("lineage").unwrap();
    let store = SessionStore::load_at(&id, temp.path()).unwrap();
    let first = complete(&store, None, None, "first", temp.path());
    complete(&store, Some(2), Some(&first), "second", temp.path());
    complete(&store, Some(2), Some(&first), "sibling", temp.path());
    let before = files(temp.path());
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                let state = SessionStore::read_transcript_at(&id, temp.path()).unwrap();
                assert_eq!(state.branches.len(), 3);
                for branch in &state.branches[1..] {
                    assert_eq!(branch.parent_branch.as_deref(), Some(first.as_str()));
                }
            });
        }
    });
    assert_eq!(before, files(temp.path()));
    // A writer immediately proceeds after rendering's reader has returned.
    complete(&store, None, None, "third", temp.path());
}

#[test]
fn reader_never_follows_session_symlinks_or_changes_permissions() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let temp = tempfile::tempdir().unwrap();
    let id = SessionId::parse("real").unwrap();
    let store = SessionStore::load_at(&id, temp.path()).unwrap();
    complete(&store, None, None, "done", temp.path());
    symlink(
        store.session_dir(),
        temp.path().join("code-rs-sessions/link"),
    )
    .unwrap();
    assert!(
        SessionStore::read_transcript_at(&SessionId::parse("link").unwrap(), temp.path()).is_err()
    );
    for (path, _, _) in files(store.session_dir()) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o400)).unwrap();
    }
    let before = files(store.session_dir());
    assert_eq!(
        SessionStore::read_transcript_at(&id, temp.path())
            .unwrap()
            .branches[0]
            .summary,
        "done"
    );
    assert_eq!(before, files(store.session_dir()));
    for (path, _, _) in before {
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o400
        );
    }
}

#[test]
fn incomplete_active_tail_is_read_without_repair_and_corruption_is_not_hidden() {
    use std::io::Write;
    let temp = tempfile::tempdir().unwrap();
    let id = SessionId::parse("tail").unwrap();
    let store = SessionStore::load_at(&id, temp.path()).unwrap();
    complete(&store, None, None, "first", temp.path());
    let dir = store.session_dir();
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("session.manifest.json")).unwrap()).unwrap();
    let segment = dir.join(manifest["current"]["segment"].as_str().unwrap());
    // Copy a real frame prefix to represent a writer interrupted during the next frame.
    let bytes = std::fs::read(&segment).unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&segment)
        .unwrap()
        .write_all(&bytes[..8])
        .unwrap();
    let before = files(temp.path());
    let state = SessionStore::read_transcript_at(&id, temp.path()).unwrap();
    assert_eq!(state.branches[0].summary, "first");
    assert_eq!(before, files(temp.path()));
    std::fs::write(dir.join("session.manifest.json"), b"{broken").unwrap();
    let before = files(temp.path());
    assert!(SessionStore::read_transcript_at(&id, temp.path()).is_err());
    assert_eq!(before, files(temp.path()));
}
