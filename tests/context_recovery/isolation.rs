//! Regression coverage at the public session and agent boundary.
use super::*;
use std::process::Command;

const CHILD_CASE: &str = "LLXPRT_CTXREC_ISOLATION_CASE";

fn trace(store: &SessionStore, cwd: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::metadata(cwd).unwrap();
        eprintln!(
            "session={} config={} workspace={} dev={} ino={} store={} handle={:p}",
            store.session_id,
            root().display(),
            cwd.display(),
            metadata.dev(),
            metadata.ino(),
            store.session_dir.display(),
            store
        );
    }
}

#[test]
fn concurrent_fixtures_keep_fixed_sessions_independent() {
    let barrier = std::sync::Barrier::new(8);
    let roots = std::thread::scope(|scope| {
        let threads: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    let cwd = workspace();
                    let config = root();
                    let sid = SessionId::parse("same-session").unwrap();
                    barrier.wait();
                    let store = SessionStore::load_at(&sid, &config).unwrap();
                    trace(&store, &cwd);
                    run_bulk_turn(&store, &cwd, "bulk.txt", "c0");
                    let reopened = reopen(&store);
                    let wrong = cwd.join("wrong");
                    std::fs::create_dir(&wrong).unwrap();
                    let error = match reserved(&reopened, None, None, "wrong", &wrong) {
                        Err(error) => error,
                        Ok(_) => panic!("a genuinely wrong workspace must fail"),
                    };
                    assert!(error.to_string().contains("session is pinned"), "{error}");
                    let request = reserved(&reopened, None, None, "continue", &cwd).unwrap();
                    let output = agent(Box::new(MockBackend::new(vec![result("done")])), &cwd)
                        .run(&reopened, &request)
                        .expect("authenticated recovery in the original workspace");
                    assert_eq!(output.status, "ok");
                    config
                })
            })
            .collect();
        threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>()
    });
    let unique: std::collections::HashSet<_> = roots.iter().collect();
    assert_eq!(unique.len(), 8);
    assert!(
        roots.iter().all(|root| !root.exists()),
        "fixture roots are cleaned up"
    );
}

// A new executable resets the fixture counters. Seed precisely the namespace
// that a recycled PID would find, without relying on the OS to recycle a PID.
#[test]
fn reused_process_root_child() {
    let Ok(case) = std::env::var(CHILD_CASE) else {
        return;
    };
    let stale = std::env::temp_dir().join(format!("llxprt-rs-ctxrec-{}", std::process::id()));
    let old_workspace = stale.join("ws-previous-run");
    std::fs::create_dir_all(&old_workspace).unwrap();
    let id = match case.as_str() {
        "checkpoint" => "checkpoint-digest-0",
        "symlink" => "symlink-vault-0",
        "entropy" => "vault-key-public-seed-a",
        _ => panic!("unknown child case"),
    };
    let sid = SessionId::parse(id).unwrap();
    let store = SessionStore::load_at(&sid, &stale).unwrap();
    eprintln!(
        "seed session={id} config={} workspace={} store={}",
        stale.display(),
        old_workspace.display(),
        store.session_dir.display()
    );
    let request = reserved(&store, None, None, "old run", &old_workspace).unwrap();
    let output = agent(
        Box::new(MockBackend::new(vec![result("done")])),
        &old_workspace,
    )
    .run(&store, &request)
    .unwrap();
    assert_eq!(output.status, "ok");
    drop(store);
    match case.as_str() {
        "checkpoint" => checkpoint_digests_cover_exactly_the_content_they_name(),
        "symlink" => symlinked_vault_artifact_fails_recovery(),
        "entropy" => vault_key_is_private_entropy_not_a_function_of_public_state(),
        _ => unreachable!(),
    }
    assert_ne!(root(), stale, "a fresh run must not adopt stale sessions");
}

#[test]
fn repeated_executables_reject_stale_pid_identity() {
    let parent = tempfile::tempdir().unwrap();
    for round in 0..2 {
        for case in ["checkpoint", "symlink", "entropy"] {
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "isolation::reused_process_root_child",
                    "--nocapture",
                ])
                .env(CHILD_CASE, case)
                .env("TMPDIR", parent.path())
                .env("TMP", parent.path())
                .env("TEMP", parent.path())
                .env("LLXPRT_CONFIG_HOME", parent.path().join("config"))
                .output()
                .unwrap();
            eprintln!(
                "round={round} case={case}\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                output.status.success(),
                "child {case} round {round}: {}",
                output.status
            );
        }
    }
}
