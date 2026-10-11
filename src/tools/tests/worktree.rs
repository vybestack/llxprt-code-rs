//! Issue 328: partial creation must never authorize Git operations in an enclosing root.
use super::*;
use std::process::Command;

fn git(root: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

fn shell(root: &std::path::Path, command: &str) -> (bool, String) {
    let mut config = cfg(root);
    config.shell.allow_shell = true;
    execute_tool(
        root,
        "run_shell_command",
        json!({"command": command, "timeout_seconds": 1}),
        &config,
    )
}

#[test]
fn timed_out_creation_preserves_dirty_root_and_verified_retry_targets_worktree() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.name", "Fixture"]);
    git(root, &["config", "user.email", "fixture@example.invalid"]);
    std::fs::write(root.join("tracked"), "committed\n").unwrap();
    git(root, &["add", "tracked"]);
    git(root, &["commit", "-m", "fixture"]);
    std::fs::remove_file(root.join("tracked")).unwrap();
    std::fs::write(root.join("untracked"), "preserve me\n").unwrap();
    let head = git(root, &["rev-parse", "HEAD"]);
    let status = git(root, &["status", "--porcelain=v1"]);

    // Deterministic interrupted-creation surrogate, not a claim that Git itself is faulty.
    // The directory exists, but creation has not reached terminal success or created .git.
    let (ok, failure) = shell(root, "mkdir incomplete; printf creation-started; sleep 3");
    assert!(!ok, "{failure}");
    assert!(failure.contains("creation-started"), "{failure}");
    assert!(failure.contains("partial filesystem effects"), "{failure}");
    assert!(failure.contains("verify repository identity"), "{failure}");
    assert!(!root.join("incomplete/.git").exists());
    assert_eq!(
        std::fs::canonicalize(
            git(root, &["-C", "incomplete", "rev-parse", "--show-toplevel"]).trim()
        )
        .unwrap(),
        std::fs::canonicalize(root).unwrap(),
        "Git really discovers the dirty enclosing repository"
    );

    // Match the prompt's fail-closed procedure. The reset is never reached.
    let guard = "test -e incomplete/.git && \
        test \"$(git -C incomplete rev-parse --show-toplevel)\" = \"$(cd incomplete && pwd -P)\" && \
        git -C incomplete reset --hard HEAD";
    let (ok, refusal) = shell(root, guard);
    assert!(!ok, "verification must refuse: {refusal}");
    std::fs::write(root.join("negative-attempt.txt"), &failure).unwrap();
    std::fs::write(root.join("verification-refusal.txt"), &refusal).unwrap();
    assert_eq!(git(root, &["rev-parse", "HEAD"]), head);
    assert!(!root.join("tracked").exists());
    assert_eq!(
        std::fs::read(root.join("untracked")).unwrap(),
        b"preserve me\n"
    );
    let after = git(root, &["status", "--porcelain=v1"]);
    for line in status.lines() {
        assert!(after.lines().any(|current| current == line), "{after}");
    }

    // A separate attempt reaches terminal success. Linked worktrees use a .git file.
    git(root, &["worktree", "add", "-b", "verified", "verified"]);
    assert!(root.join("verified/.git").is_file());
    let (ok, identity) = shell(
        root,
        "test -e verified/.git && \
        test \"$(git -C verified rev-parse --show-toplevel)\" = \"$(cd verified && pwd -P)\" && \
        test \"$(git -C verified rev-parse --git-common-dir)\" = \"$(pwd -P)/.git\" && \
        test \"$(git -C verified branch --show-current)\" = verified && \
        test \"$(git -C verified rev-parse HEAD)\" = \"$(git rev-parse HEAD)\" && \
        git -C verified reset --hard HEAD",
    );
    assert!(ok, "{identity}");
    assert!(root.join("verified/tracked").exists());
    assert_eq!(git(root, &["rev-parse", "HEAD"]), head);
    assert!(!root.join("tracked").exists());
    assert_eq!(
        std::fs::read(root.join("untracked")).unwrap(),
        b"preserve me\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("negative-attempt.txt")).unwrap(),
        failure
    );
}

#[test]
fn timeout_safety_diagnostic_precedes_large_output_and_respects_tiny_caps() {
    let fixture = tempfile::tempdir().unwrap();
    for cap in [32, 512] {
        let mut config = cfg(fixture.path());
        config.shell.allow_shell = true;
        config.shell.max_shell_output = cap;
        let (ok, output) = execute_tool(
            fixture.path(),
            "run_shell_command",
            json!({"command":"while :; do printf payload; done", "timeout_seconds":1}),
            &config,
        );
        assert!(!ok, "{output}");
        assert!(output.len() <= cap, "{} > {cap}", output.len());
        assert!(output.starts_with("comm"), "{output}");
        if cap == 512 {
            assert!(output.starts_with("command timed out"), "{output}");
            assert!(output.contains("partial filesystem effects"), "{output}");
            assert!(output.contains("verify repository identity"), "{output}");
        }
    }
}
