//! Manifest-selected artifacts must stay single local basenames for both callers.
use llxprt_code_rs::session::{RoundRecord, SessionId, SessionStore, StoreError};
use serde_json::Value;
use std::os::unix::fs::{symlink, MetadataExt};
use std::path::{Path, PathBuf};

fn fixture(root: &Path) -> (SessionId, PathBuf, Value) {
    let id = SessionId::parse("confined").unwrap();
    let store = SessionStore::load_at(&id, root).unwrap();
    let request = store.start_request(None, None, "prompt", root).unwrap();
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
    store.replace_snapshot(&store.snapshot().unwrap()).unwrap();
    let directory = store.session_dir().to_path_buf();
    let manifest =
        serde_json::from_slice(&std::fs::read(directory.join("session.manifest.json")).unwrap())
            .unwrap();
    (id, directory, manifest)
}

// Do not follow links while measuring bytes and non-atime metadata.
fn fingerprint(root: &Path) -> Vec<(PathBuf, Vec<u8>, Vec<u64>)> {
    let mut rows = vec![];
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        let bytes = if metadata.is_file() {
            std::fs::read(&path).unwrap()
        } else if metadata.file_type().is_symlink() {
            std::fs::read_link(&path)
                .unwrap()
                .as_os_str()
                .as_encoded_bytes()
                .to_vec()
        } else {
            rows.extend(fingerprint(&path));
            vec![]
        };
        rows.push((
            path,
            bytes,
            vec![
                metadata.mode().into(),
                metadata.ino(),
                metadata.len(),
                metadata.mtime() as u64,
                metadata.mtime_nsec() as u64,
                metadata.ctime() as u64,
                metadata.ctime_nsec() as u64,
                metadata.uid().into(),
                metadata.gid().into(),
                metadata.nlink(),
            ],
        ));
    }
    rows.sort();
    rows
}

fn publish(directory: &Path, manifest: &Value) {
    std::fs::write(
        directory.join("session.manifest.json"),
        serde_json::to_vec(manifest).unwrap(),
    )
    .unwrap();
}

fn assert_bad_name(error: StoreError) {
    assert!(matches!(error, StoreError::Corrupt(_)), "{error}");
    assert_eq!(
        error.to_string(),
        "corrupt session state: session manifest artifact name must be a single local basename"
    );
}

#[test]
fn all_manifest_names_are_validated_before_current_or_retained_selection() {
    let root = tempfile::tempdir().unwrap();
    let (id, directory, original) = fixture(root.path());
    for set in ["current", "previous"] {
        for artifact in ["snapshot", "segment"] {
            let basename = original[set][artifact].as_str().unwrap();
            let outside = root.path().join(basename);
            std::fs::copy(directory.join(basename), &outside).unwrap();
            let redirect = directory.join("redirect");
            symlink(root.path(), &redirect).unwrap();
            let names = [
                String::new(),
                ".".into(),
                "..".into(),
                outside.to_str().unwrap().into(),
                format!("../{basename}"),
                format!("./{basename}"),
                format!("redirect/{basename}"),
                format!("nested/path/{basename}"),
                format!("{basename}/"),
                format!("redirect\\{basename}"),
                format!("{basename}\0"),
            ];
            for name in names {
                let mut manifest = original.clone();
                manifest[set][artifact] = name.clone().into();
                publish(&directory, &manifest);
                let before = fingerprint(root.path());
                assert_bad_name(SessionStore::read_transcript_at(&id, root.path()).unwrap_err());
                assert_eq!(
                    before,
                    fingerprint(root.path()),
                    "{set}.{artifact}={name:?}"
                );
                // Opening a writer retains its existing chmod contract. Measure
                // after open, then ensure the rejected load cannot repair or fall back.
                let writer = SessionStore::load_at(&id, root.path()).unwrap();
                let before = fingerprint(root.path());
                assert_bad_name(writer.snapshot().unwrap_err());
                assert_eq!(
                    before,
                    fingerprint(root.path()),
                    "{set}.{artifact}={name:?}"
                );
            }
            std::fs::remove_file(redirect).unwrap();
        }
    }
}

#[test]
fn safe_local_names_preserve_read_only_and_writable_retained_recovery() {
    let root = tempfile::tempdir().unwrap();
    let (id, directory, mut manifest) = fixture(root.path());
    // No speculative generated-name grammar: ordinary local basenames are valid.
    for set in ["current", "previous"] {
        for artifact in ["snapshot", "segment"] {
            let old = manifest[set][artifact].as_str().unwrap();
            let new = format!("local.{set}.{artifact}");
            std::fs::rename(directory.join(old), directory.join(&new)).unwrap();
            manifest[set][artifact] = new.into();
        }
    }
    publish(&directory, &manifest);
    let before = fingerprint(root.path());
    let read = SessionStore::read_transcript_at(&id, root.path()).unwrap();
    assert_eq!(read.branches[0].summary, "done");
    assert_eq!(before, fingerprint(root.path()));
    let writer = SessionStore::load_at(&id, root.path()).unwrap();
    assert_eq!(writer.snapshot().unwrap().branches[0].summary, "done");
    drop(writer);
    std::fs::write(
        directory.join(manifest["current"]["snapshot"].as_str().unwrap()),
        b"broken snapshot",
    )
    .unwrap();
    let before = fingerprint(root.path());
    assert_eq!(
        SessionStore::read_transcript_at(&id, root.path())
            .unwrap()
            .branches[0]
            .summary,
        "done"
    );
    assert_eq!(before, fingerprint(root.path()));
    let writer = SessionStore::load_at(&id, root.path()).unwrap();
    assert_eq!(writer.snapshot().unwrap().branches[0].summary, "done");
    assert_ne!(
        std::fs::read(directory.join("session.manifest.json")).unwrap(),
        serde_json::to_vec(&manifest).unwrap()
    );
}

#[test]
fn final_artifact_symlinks_remain_rejected_in_current_and_retained_sets() {
    for set in ["current", "previous"] {
        for artifact in ["snapshot", "segment"] {
            let root = tempfile::tempdir().unwrap();
            let (id, directory, mut manifest) = fixture(root.path());
            let basename = manifest[set][artifact].as_str().unwrap();
            let path = directory.join(basename);
            let outside = root.path().join(basename);
            std::fs::rename(&path, &outside).unwrap();
            symlink(&outside, &path).unwrap();
            if set == "current" {
                manifest["previous"] = Value::Null;
            } else {
                std::fs::write(
                    directory.join(manifest["current"]["snapshot"].as_str().unwrap()),
                    b"broken snapshot",
                )
                .unwrap();
            }
            publish(&directory, &manifest);
            let before = fingerprint(root.path());
            assert!(SessionStore::read_transcript_at(&id, root.path()).is_err());
            assert_eq!(before, fingerprint(root.path()));
            let writer = SessionStore::load_at(&id, root.path()).unwrap();
            let before = fingerprint(root.path());
            assert!(writer.snapshot().is_err());
            assert_eq!(before, fingerprint(root.path()));
        }
    }
}
