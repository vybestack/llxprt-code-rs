use super::*;
use std::path::Path;

fn assert_no_private_stage_bytes(dir: &Path) {
    assert!(
        std::fs::read_dir(dir).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".stage.")
        }),
        "staging residue remained"
    );
}

/// A second writer for the same logical artifact set must fail without replacing or deleting
/// the completed set owned by the first writer.
#[test]
fn colliding_save_preserves_existing_completed_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    let mut first = test_result(true);
    first.raw_stdout = b"first-stdout".to_vec();
    first.stderr = b"first-stderr".to_vec();
    save_turn(dir.path(), "starter", "same-session", 1, &first).unwrap();

    let mut second = test_result(true);
    second.raw_stdout = b"second-stdout".to_vec();
    second.stderr = b"second-stderr".to_vec();
    assert!(save_turn(dir.path(), "starter", "same-session", 1, &second).is_err());

    let scenario_dir = dir.path().join("starter");
    assert_eq!(
        std::fs::read(scenario_dir.join("same-session.turn1.json")).unwrap(),
        b"first-stdout"
    );
    assert_eq!(
        std::fs::read(scenario_dir.join("same-session.turn1.stderr")).unwrap(),
        b"first-stderr"
    );
    assert!(scenario_dir.join("same-session.turn1.meta.json").is_file());
    assert!(scenario_dir.join("same-session.turn1.done").is_file());
    assert_no_private_stage_bytes(&scenario_dir);
}

#[cfg(target_os = "macos")]
#[test]
fn macos_stage_is_unlinked_before_private_bytes_are_written() {
    let temp = tempfile::tempdir().unwrap();
    let dir = crate::tools::open_root(temp.path()).unwrap();
    let candidate = stage_at(&dir, "final", b"verified bytes").unwrap();
    assert!(std::fs::read_dir(temp.path()).unwrap().next().is_none());

    publish_stage_at(&dir, &candidate, "final").unwrap();
    assert_eq!(
        std::fs::read(temp.path().join("final")).unwrap(),
        b"verified bytes"
    );
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
}

#[cfg(target_os = "macos")]
#[test]
fn macos_crash_after_staging_leaves_no_stage_entry() {
    const CHILD_DIR: &str = "LLXPRT_TEST_CRASH_AFTER_ARTIFACT_STAGE";
    if let Some(path) = std::env::var_os(CHILD_DIR) {
        let dir = crate::tools::open_root(Path::new(&path)).unwrap();
        let _candidate = stage_at(&dir, "final", b"private staged bytes").unwrap();
        std::process::abort();
    }

    let temp = tempfile::tempdir().unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "harness::tests::publication::macos_crash_after_staging_leaves_no_stage_entry",
            "--nocapture",
        ])
        .env(CHILD_DIR, temp.path())
        .status()
        .unwrap();
    assert!(!status.success(), "the staging child must abort");
    assert_no_private_stage_bytes(temp.path());
}

#[test]
fn retained_scenario_directory_prevents_parent_substitution() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("output");
    std::fs::create_dir(&output).unwrap();
    let root = crate::tools::open_root(&output).unwrap();
    let scenario = ensure_artifact_subdir(&root, "scenario").unwrap();
    let moved = temp.path().join("moved-scenario");
    std::fs::rename(output.join("scenario"), &moved).unwrap();
    std::fs::create_dir(output.join("scenario")).unwrap();
    std::fs::write(output.join("scenario/final"), b"replacement victim").unwrap();

    let candidate = stage_at(&scenario, "final", b"verified bytes").unwrap();
    publish_stage_at(&scenario, &candidate, "final").unwrap();
    assert_eq!(
        std::fs::read(moved.join("final")).unwrap(),
        b"verified bytes"
    );
    assert_eq!(
        std::fs::read(output.join("scenario/final")).unwrap(),
        b"replacement victim"
    );
}

/// A forced mid-save failure may leave descriptor-verified final files, but never installs the
/// completion marker or retains private staging bytes.
#[test]
fn forced_mid_save_failure_never_marks_partial_artifacts_complete() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    let scenario_dir = base.join("pong");
    std::fs::create_dir_all(&scenario_dir).unwrap();
    std::fs::create_dir(scenario_dir.join("pong-sess.turn1.stderr")).unwrap();
    let result = test_result(true);
    assert!(save_turn(base, "pong", "pong-sess", 1, &result).is_err());
    assert_eq!(
        std::fs::read(scenario_dir.join("pong-sess.turn1.json")).unwrap(),
        result.raw_stdout
    );
    assert!(!scenario_dir.join("pong-sess.turn1.done").exists());
    assert_no_private_stage_bytes(&scenario_dir);
}

/// A forced third publication failure leaves earlier finals as incomplete evidence, without a
/// completion marker or private staging bytes.
#[test]
fn forced_third_write_failure_leaves_only_unmarked_verified_finals() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    let scenario_dir = base.join("flappy");
    std::fs::create_dir_all(&scenario_dir).unwrap();
    std::fs::create_dir(scenario_dir.join("flappy-sess.turn1.meta.json")).unwrap();
    let mut result = test_result(true);
    result.raw_stdout = b"raw-fake-out".to_vec();
    result.stderr = b"raw-fake-err".to_vec();
    assert!(save_turn(base, "flappy", "flappy-sess", 1, &result).is_err());
    assert_eq!(
        std::fs::read(scenario_dir.join("flappy-sess.turn1.json")).unwrap(),
        result.raw_stdout
    );
    assert_eq!(
        std::fs::read(scenario_dir.join("flappy-sess.turn1.stderr")).unwrap(),
        result.stderr
    );
    assert!(!scenario_dir.join("flappy-sess.turn1.done").exists());
    assert_no_private_stage_bytes(&scenario_dir);
}

/// A forced marker placement failure leaves the three verified finals but no valid completion
/// marker and no private staging bytes.
#[test]
fn forced_marker_placement_failure_leaves_only_unmarked_finals() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    let scenario_dir = base.join("starter");
    std::fs::create_dir_all(&scenario_dir).unwrap();
    std::fs::create_dir(scenario_dir.join("starter-sess.turn1.done")).unwrap();
    let mut result = test_result(true);
    result.raw_stdout = b"raw-out".to_vec();
    result.stderr = b"raw-err".to_vec();
    assert!(save_turn(base, "starter", "starter-sess", 1, &result).is_err());
    assert_eq!(
        std::fs::read(scenario_dir.join("starter-sess.turn1.json")).unwrap(),
        result.raw_stdout
    );
    assert_eq!(
        std::fs::read(scenario_dir.join("starter-sess.turn1.stderr")).unwrap(),
        result.stderr
    );
    assert!(scenario_dir.join("starter-sess.turn1.meta.json").is_file());
    assert!(scenario_dir.join("starter-sess.turn1.done").is_dir());
    assert_no_private_stage_bytes(&scenario_dir);
}
