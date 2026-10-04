//! Descriptor-relative path and symlink confinement witnesses.
use super::*;

#[test]
fn rejects_absolute_path() {
    let d = tempfile::tempdir().unwrap();
    assert!(!run(d.path(), "read_file", json!({"path": "/etc/hosts"})).0);
    assert!(
        !run(
            d.path(),
            "write_file",
            json!({"path": "/tmp/x.txt", "content": "x"})
        )
        .0
    );
}

#[test]
fn rejects_parent_dotdot_escape() {
    let d = tempfile::tempdir().unwrap();
    assert!(!run(d.path(), "read_file", json!({"path": "../escape"})).0);
    assert!(
        !run(
            d.path(),
            "write_file",
            json!({"path": "a/../../escape", "content": "x"})
        )
        .0
    );
    // No outside side effect: parent tempdir has no `escape` file.
    let parent = d.path().parent().unwrap().join("escape");
    assert!(!parent.exists());
}

#[test]
fn dangling_final_symlink_is_rejected() {
    let d = tempfile::tempdir().unwrap();
    let target = d.path().join("does-not-exist");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, d.path().join("dangling")).unwrap();
    let (ok, msg) = run(d.path(), "read_file", json!({"path": "dangling"}));
    assert!(!ok, "dangling final symlink must be rejected: {msg}");
}

#[test]
fn intermediate_symlink_escape_has_no_outside_side_effect() {
    let d = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_file = outside.path().join("secret.txt");
    std::fs::write(&outside_file, "original").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), d.path().join("esc")).unwrap();

    let (ok, _) = run(d.path(), "read_file", json!({"path": "esc/secret.txt"}));
    assert!(!ok);
    let (ok, _) = run(
        d.path(),
        "write_file",
        json!({"path": "esc/new.txt", "content": "x"}),
    );
    assert!(!ok);
    let outside_file_after = std::fs::read_to_string(&outside_file).unwrap();
    assert_eq!(outside_file_after, "original");
    assert!(!outside.path().join("new.txt").exists());
}

#[test]
fn symlink_final_target_is_rejected_and_no_recursion() {
    let d = tempfile::tempdir().unwrap();
    let real = d.path().join("real.txt");
    std::fs::write(&real, "hello").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real, d.path().join("alias")).unwrap();
    let (ok, msg) = run(d.path(), "read_file", json!({"path": "alias"}));
    assert!(!ok, "final symlink target must be rejected: {msg}");
}
