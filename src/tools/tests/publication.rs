use super::*;

#[test]
fn atomic_write_then_read_roundtrip() {
    let d = tempfile::tempdir().unwrap();
    let (ok, m) = run(
        d.path(),
        "write_file",
        json!({"path": "a/b/c.txt", "content": "hello ü"}),
    );
    assert!(ok, "write failed: {m}");
    let content = std::fs::read_to_string(d.path().join("a/b/c.txt")).unwrap();
    assert_eq!(content, "hello ü");
    let (ok, body) = run(d.path(), "read_file", json!({"path": "a/b/c.txt"}));
    assert!(ok, "read failed: {body}");
    assert!(body.contains("hello ü"));
}

#[test]
fn substituted_write_stage_is_rejected_without_deleting_replacement() {
    let temp = tempfile::tempdir().unwrap();
    let root = open_root(temp.path()).unwrap();
    let base = temp.path().to_path_buf();
    install_stage_substitution_hook(Some(Box::new(move |name| {
        std::fs::rename(base.join(name), base.join("moved-stage")).unwrap();
        std::fs::write(base.join(name), b"replacement victim").unwrap();
    })));

    let error = atomic_write_into(&root, "final", b"private intended bytes").unwrap_err();
    assert!(error.contains("staging file identity changed"), "{error}");
    assert!(!temp.path().join("final").exists());
    let victim = std::fs::read_dir(temp.path())
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".llxprt-tmp-")
        })
        .unwrap();
    assert_eq!(std::fs::read(victim.path()).unwrap(), b"replacement victim");
    assert_eq!(
        std::fs::metadata(temp.path().join("moved-stage"))
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn exact_copy_stage_substitution_is_rejected_by_descriptor_identity() {
    let temp = tempfile::tempdir().unwrap();
    let root = open_root(temp.path()).unwrap();
    let base = temp.path().to_path_buf();
    install_stage_substitution_hook(Some(Box::new(move |name| {
        std::fs::rename(base.join(name), base.join("moved-stage")).unwrap();
        std::fs::write(base.join(name), b"private intended bytes").unwrap();
    })));

    let error = atomic_write_into(&root, "final", b"private intended bytes").unwrap_err();
    assert!(error.contains("staging file identity changed"), "{error}");
    assert!(!temp.path().join("final").exists());
    assert_eq!(
        std::fs::metadata(temp.path().join("moved-stage"))
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn symlink_stage_substitution_is_rejected_without_touching_target() {
    let temp = tempfile::tempdir().unwrap();
    let root = open_root(temp.path()).unwrap();
    std::fs::write(temp.path().join("victim"), b"victim bytes").unwrap();
    let base = temp.path().to_path_buf();
    install_stage_substitution_hook(Some(Box::new(move |name| {
        std::fs::rename(base.join(name), base.join("moved-stage")).unwrap();
        std::os::unix::fs::symlink("victim", base.join(name)).unwrap();
    })));

    assert!(atomic_write_into(&root, "final", b"private intended bytes").is_err());
    assert_eq!(
        std::fs::read(temp.path().join("victim")).unwrap(),
        b"victim bytes"
    );
    assert_eq!(
        std::fs::metadata(temp.path().join("moved-stage"))
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn fifo_stage_substitution_is_rejected_without_blocking() {
    use std::os::unix::ffi::OsStrExt as _;

    let temp = tempfile::tempdir().unwrap();
    let root = open_root(temp.path()).unwrap();
    let base = temp.path().to_path_buf();
    install_stage_substitution_hook(Some(Box::new(move |name| {
        std::fs::rename(base.join(name), base.join("moved-stage")).unwrap();
        let fifo = base.join(name);
        let fifo = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    })));

    assert!(atomic_write_into(&root, "final", b"private intended bytes").is_err());
    assert!(!temp.path().join("final").exists());
    assert_eq!(
        std::fs::metadata(temp.path().join("moved-stage"))
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn destination_symlink_substitution_is_post_install_and_preserves_target() {
    let temp = tempfile::tempdir().unwrap();
    let root = open_root(temp.path()).unwrap();
    std::fs::write(temp.path().join("victim"), b"victim bytes").unwrap();
    let base = temp.path().to_path_buf();
    install_publication_hook(
        PublicationHookPoint::AfterRename,
        Box::new(move |leaf| {
            std::fs::rename(base.join(leaf), base.join("moved-installed")).unwrap();
            std::os::unix::fs::symlink("victim", base.join(leaf)).unwrap();
        }),
    );

    let error = atomic_write_into(&root, "final", b"private intended bytes").unwrap_err();
    assert!(error.contains("installed final, but durability or integrity is unknown"));
    assert_eq!(
        std::fs::read(temp.path().join("victim")).unwrap(),
        b"victim bytes"
    );
    assert_eq!(
        std::fs::metadata(temp.path().join("moved-installed"))
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn destination_directory_substitution_is_post_install_and_preserves_entry() {
    let temp = tempfile::tempdir().unwrap();
    let root = open_root(temp.path()).unwrap();
    let base = temp.path().to_path_buf();
    install_publication_hook(
        PublicationHookPoint::AfterRename,
        Box::new(move |leaf| {
            std::fs::rename(base.join(leaf), base.join("moved-installed")).unwrap();
            std::fs::create_dir(base.join(leaf)).unwrap();
        }),
    );

    let error = atomic_write_into(&root, "final", b"private intended bytes").unwrap_err();
    assert!(error.contains("installed final, but durability or integrity is unknown"));
    assert!(temp.path().join("final").is_dir());
    assert_eq!(
        std::fs::metadata(temp.path().join("moved-installed"))
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn directory_sync_failure_is_explicitly_post_install() {
    let temp = tempfile::tempdir().unwrap();
    let root = open_root(temp.path()).unwrap();
    fail_next_directory_sync();

    let error = atomic_write_into(&root, "final", b"installed bytes").unwrap_err();
    assert!(
        error.contains("installed final, but durability or integrity is unknown"),
        "{error}"
    );
    assert_eq!(
        std::fs::read(temp.path().join("final")).unwrap(),
        b"installed bytes"
    );
}

fn assert_post_install_substitution(point: PublicationHookPoint) {
    let temp = tempfile::tempdir().unwrap();
    let root = open_root(temp.path()).unwrap();
    let base = temp.path().to_path_buf();
    install_publication_hook(
        point,
        Box::new(move |leaf| {
            std::fs::rename(base.join(leaf), base.join("moved-installed")).unwrap();
            std::fs::write(base.join(leaf), b"replacement victim").unwrap();
        }),
    );

    let error = atomic_write_into(&root, "final", b"private intended bytes").unwrap_err();
    assert!(
        error.contains("installed final, but durability or integrity is unknown"),
        "{error}"
    );
    assert_eq!(
        std::fs::read(temp.path().join("final")).unwrap(),
        b"replacement victim"
    );
    assert_eq!(
        std::fs::metadata(temp.path().join("moved-installed"))
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn substitution_immediately_after_rename_is_post_install_and_preserves_victim() {
    assert_post_install_substitution(PublicationHookPoint::AfterRename);
}

#[test]
fn substitution_after_directory_sync_is_detected_and_preserves_victim() {
    assert_post_install_substitution(PublicationHookPoint::AfterDirectorySync);
}

#[test]
fn substitution_before_directory_sync_is_detected_and_preserves_victim() {
    assert_post_install_substitution(PublicationHookPoint::BeforeDirectorySync);
}
