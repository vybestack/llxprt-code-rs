//! Read-only selection must not publish the retained recovery set.
use llxprt_code_rs::session::{RoundRecord, SessionId, SessionStore};

#[test]
fn retained_set_is_selected_without_manifest_repair() {
    let root = tempfile::tempdir().unwrap();
    let id = SessionId::parse("retained").unwrap();
    let store = SessionStore::load_at(&id, root.path()).unwrap();
    let request = store
        .start_request(None, None, "prompt", root.path())
        .unwrap();
    store
        .finalize(
            &request,
            "done",
            &[RoundRecord {
                assistant: "done".into(),
                calls: vec![],
            }],
        )
        .unwrap();
    let state = store.snapshot().unwrap();
    store.replace_snapshot(&state).unwrap();
    let manifest_path = store.session_dir().join("session.manifest.json");
    let manifest_bytes = std::fs::read(&manifest_path).unwrap();
    let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes).unwrap();
    assert!(manifest["previous"].is_object());
    let current = store
        .session_dir()
        .join(manifest["current"]["snapshot"].as_str().unwrap());
    std::fs::write(&current, b"broken snapshot").unwrap();
    let before: Vec<_> = std::fs::read_dir(store.session_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_file())
        .map(|path| {
            (
                path.clone(),
                std::fs::read(&path).unwrap(),
                std::fs::metadata(&path).unwrap().modified().unwrap(),
            )
        })
        .collect();
    let read = SessionStore::read_transcript_at(&id, root.path()).unwrap();
    assert_eq!(read.branches[0].summary, "done");
    for (path, bytes, modified) in before {
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(
            std::fs::metadata(path).unwrap().modified().unwrap(),
            modified
        );
    }
    assert_eq!(std::fs::read(manifest_path).unwrap(), manifest_bytes);
}
