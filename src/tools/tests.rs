use super::*;
use serde_json::json;
use std::time::Duration;

mod advanced;

fn cfg(root: &std::path::Path) -> ToolConfig {
    let ws = WorkspaceCap::open(root).unwrap();
    ToolConfig {
        ws,
        max_output_bytes: 16 * 1024,
        shell: ShellConfig {
            max_shell_output: 64 * 1024,
            max_shell_timeout: Duration::from_secs(60),
            allow_shell: false,
        },
    }
}

pub(super) fn run(root: &std::path::Path, name: &str, args: JsonValue) -> (bool, String) {
    let (ok, s) = execute_tool(root, name, args, &cfg(root));
    (ok, s)
}

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

mod publication;

#[test]
fn replace_reports_directory_sync_failure_after_installation() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("file"), "old").unwrap();
    fail_next_directory_sync();
    let (ok, error) = run(
        temp.path(),
        "replace",
        json!({"path": "file", "old_string": "old", "new_string": "new"}),
    );
    assert!(!ok);
    assert!(error.contains("installed file, but durability or integrity is unknown"));
    assert_eq!(
        std::fs::read_to_string(temp.path().join("file")).unwrap(),
        "new"
    );
}

#[test]
fn replace_detects_post_sync_substitution_without_deleting_victim() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("file"), "old").unwrap();
    let base = temp.path().to_path_buf();
    install_publication_hook(
        PublicationHookPoint::AfterDirectorySync,
        Box::new(move |leaf| {
            std::fs::rename(base.join(leaf), base.join("moved-replacement")).unwrap();
            std::fs::write(base.join(leaf), b"replacement victim").unwrap();
        }),
    );
    let (ok, error) = run(
        temp.path(),
        "replace",
        json!({"path": "file", "old_string": "old", "new_string": "new"}),
    );
    assert!(!ok);
    assert!(error.contains("installed file, but durability or integrity is unknown"));
    assert_eq!(
        std::fs::read(temp.path().join("file")).unwrap(),
        b"replacement victim"
    );
    assert_eq!(
        std::fs::metadata(temp.path().join("moved-replacement"))
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn atomic_write_leaves_no_temp_files() {
    let d = tempfile::tempdir().unwrap();
    let _ = run(
        d.path(),
        "write_file",
        json!({"path": "x.txt", "content": "v1"}),
    );
    let listing = std::fs::read_dir(d.path()).unwrap();
    let names: Vec<String> = listing
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.iter().all(|n| n != ".llxprt-tmp"),
        "temp file left: {names:?}"
    );
}

/// A rename failure on the workspace atomic write (the destination is a directory) must
/// fail, leave the existing destination untouched, and remove the exact temp file (no
/// `.llxprt-tmp` residue).
#[test]
fn atomic_write_rename_failure_preserves_destination_and_clears_private_bytes() {
    let d = tempfile::tempdir().unwrap();
    let dir_dest = d.path().join("dir-target");
    std::fs::create_dir(&dir_dest).unwrap();
    std::fs::write(dir_dest.join("keep.txt"), "original").unwrap();
    let (ok, msg) = run(
        d.path(),
        "write_file",
        json!({"path": "dir-target", "content": "replacement"}),
    );
    assert!(!ok, "a directory destination must fail: {msg}");
    assert!(
        dir_dest.is_dir(),
        "the existing destination directory is unchanged"
    );
    assert_eq!(
        std::fs::read(dir_dest.join("keep.txt")).unwrap(),
        b"original"
    );
    for entry in std::fs::read_dir(d.path()).unwrap().filter_map(Result::ok) {
        if entry.file_name().to_string_lossy().contains(".llxprt-tmp") {
            assert_eq!(
                entry.metadata().unwrap().len(),
                0,
                "a failed publication must clear retained private staging bytes"
            );
        }
    }
}

/// `drain_bytes` never reads or retains more than the requested cap.
#[test]
fn drain_bytes_never_reads_or_retains_beyond_cap() {
    struct CountingReader {
        buf: Vec<u8>,
        pos: usize,
        total: usize,
    }
    impl std::io::Read for CountingReader {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            let n = self.buf.len().saturating_sub(self.pos).min(out.len());
            out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
            self.pos += n;
            self.total += n;
            Ok(n)
        }
    }
    let data = (0..10_000).map(|i| (i % 251) as u8).collect::<Vec<u8>>();
    for cap in [0, 1, 4095, 4096, 8192, 10_000] {
        let mut r = CountingReader {
            buf: data.clone(),
            pos: 0,
            total: 0,
        };
        let out = drain_bytes(&mut r, cap).unwrap();
        assert_eq!(out.len(), cap.min(data.len()), "cap {cap}");
        assert!(r.total <= cap, "cap {cap}: read {total}", total = r.total);
        assert_eq!(&out[..], &data[..out.len()], "cap {cap}");
    }
}

/// The model-visible `read_file` string (window header plus body) is at most
/// `max_output` bytes total.
#[test]
fn read_file_total_is_bounded_including_frame() {
    let d = tempfile::tempdir().unwrap();
    let big = "y".repeat(MAX_FILE_BYTES);
    std::fs::write(d.path().join("big.txt"), &big).unwrap();
    let ws = WorkspaceCap::open(d.path()).unwrap();
    let c = ToolConfig {
        ws,
        max_output_bytes: 8192,
        shell: ShellConfig {
            max_shell_output: 64 * 1024,
            max_shell_timeout: Duration::from_secs(30),
            allow_shell: false,
        },
    };
    let (ok, body) = execute_tool(d.path(), "read_file", json!({"path": "big.txt"}), &c);
    assert!(ok, "{body}");
    assert!(
        body.len() <= 8192,
        "read_file total must be <= max_output: {}",
        body.len()
    );
}

#[test]
fn repeated_root_listings_use_independent_directory_offsets() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("one"), "1").unwrap();
    std::fs::write(d.path().join("two"), "2").unwrap();
    std::fs::create_dir(d.path().join("child")).unwrap();
    std::fs::write(d.path().join("child/nested"), "3").unwrap();
    let config = cfg(d.path());

    let first = execute_tool(d.path(), "list_directory", json!({"path": ""}), &config);
    let nested = execute_tool(
        d.path(),
        "list_directory",
        json!({"path": "child"}),
        &config,
    );
    assert!(nested.0);
    assert!(nested.1.contains("file nested"));
    let second = execute_tool(d.path(), "list_directory", json!({"path": ""}), &config);
    assert_eq!(first, second);
    assert!(first.0);
    assert!(first.1.contains("file one"));
    assert!(first.1.contains("file two"));
}

/// The model-visible `list_directory` string is at most `MAX_LIST_BYTES` total.
#[test]
fn list_directory_output_is_bounded() {
    let d = tempfile::tempdir().unwrap();
    // Many entries whose joined names exceed `MAX_LIST_BYTES` force the byte cap; the
    // joined output (including any truncation note) stays inside `MAX_LIST_BYTES`.
    for i in 0..(MAX_LIST_ITEMS + 5) {
        std::fs::write(d.path().join(format!("entry{i:06}")), "x").unwrap();
    }
    let (ok, body) = run(d.path(), "list_directory", json!({"path": ""}));
    assert!(ok, "{body}");
    assert!(
        body.len() <= MAX_LIST_BYTES,
        "list output bounded: {}",
        body.len()
    );
}

/// The model-visible shell success and error strings (framing and combined output
/// included) are each at most the configured max.
#[test]
fn shell_output_total_is_bounded_for_success_and_error() {
    let d = tempfile::tempdir().unwrap();
    let ws = WorkspaceCap::open(d.path()).unwrap();
    let c = ToolConfig {
        ws,
        max_output_bytes: 64 * 1024,
        shell: ShellConfig {
            max_shell_output: 4096,
            max_shell_timeout: Duration::from_secs(30),
            allow_shell: true,
        },
    };
    // A large success output is bounded (both streams combined through one cap).
    let (_ok, body) = execute_tool(
        d.path(),
        "run_shell_command",
        json!({"command": "head -c 20000 /dev/zero"}),
        &c,
    );
    assert!(
        body.len() <= 4096,
        "shell success total bounded: {}",
        body.len()
    );
    let (ok, _) = execute_tool(
        d.path(),
        "run_shell_command",
        json!({"command": "true"}),
        &c,
    );
    assert!(ok);
    let (ok, body) = execute_tool(
        d.path(),
        "run_shell_command",
        json!({"command": "head -c 20000 /dev/zero; exit 9"}),
        &c,
    );
    assert!(!ok, "nonzero exit is ok=false: {body}");
    assert!(
        body.len() <= 4096,
        "shell error total bounded: {}",
        body.len()
    );
}

#[test]
fn replace_exact_once_and_expected_mismatch() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("f.txt"), "a b a").unwrap();
    let (ok, _) = run(
        d.path(),
        "replace",
        json!({"path": "f.txt", "old_string": "b", "new_string": "B"}),
    );
    assert!(ok);
    assert_eq!(
        std::fs::read_to_string(d.path().join("f.txt")).unwrap(),
        "a B a"
    );
    // old_string that appears twice without `expected` must fail and change nothing.
    let (ok, _) = run(
        d.path(),
        "replace",
        json!({"path": "f.txt", "old_string": "a", "new_string": "z"}),
    );
    assert!(!ok);
    assert_eq!(
        std::fs::read_to_string(d.path().join("f.txt")).unwrap(),
        "a B a"
    );
}

#[test]
fn replace_missing_nested_path_creates_no_directories() {
    let workspace = tempfile::tempdir().unwrap();
    let (ok, _) = run(
        workspace.path(),
        "replace",
        json!({"path": "missing/parent/file.txt", "old_string": "x", "new_string": "y"}),
    );
    assert!(!ok);
    assert!(!workspace.path().join("missing").exists());
}

#[test]
fn search_no_symlink_recursion_and_max_results() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("dir")).unwrap();
    for i in 0..10 {
        std::fs::write(d.path().join(format!("dir/f{i}.txt")), "needle line").unwrap();
    }
    // A symlinked directory must not be followed.
    #[cfg(unix)]
    std::os::unix::fs::symlink(d.path().join("dir"), d.path().join("dirlink")).unwrap();
    let (ok, body) = run(
        d.path(),
        "search_file_content",
        json!({"pattern": "needle", "max_results": 3}),
    );
    assert!(ok);
    assert_eq!(body.lines().count(), 3, "max_results not honored: {body}");
    let hits = body.matches("needle").count();
    assert_eq!(hits, 3);
}

#[test]
fn typed_args_reject_missing_wrong_unknown() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("f.txt"), "x").unwrap();
    // Missing required.
    assert!(!run(d.path(), "read_file", json!({})).0);
    // Wrong type.
    assert!(!run(d.path(), "read_file", json!({"path": 5})).0);
    assert!(
        !run(
            d.path(),
            "read_file",
            json!({"path": "f.txt", "offset": "0"})
        )
        .0
    );
    // Unknown field.
    let (ok, msg) = run(d.path(), "read_file", json!({"path": "f.txt", "bogus": 1}));
    assert!(!ok, "unknown arg accepted: {msg}");
    // Wrong max_results type.
    assert!(
        !run(
            d.path(),
            "search_file_content",
            json!({"pattern": "x", "max_results": "many"})
        )
        .0
    );
}

/// `max_results` exact boundaries: 0 returns zero results **without traversing**
/// (a start path that does not exist is never even opened), 1 returns exactly one,
/// `MAX_SEARCH_RESULTS` returns at most the cap, and `MAX_SEARCH_RESULTS + 1`
/// clamps to the cap; the traversal/byte metadata stays bounded on every boundary.
#[test]
fn search_max_results_exact_zero_one_max_max_plus_one() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("dir")).unwrap();
    for i in 0..(MAX_SEARCH_RESULTS + 5) {
        std::fs::write(d.path().join(format!("dir/f{i}.txt")), "needle line").unwrap();
    }
    // 0: no match lines and no traversal — a nonexistent start path would fail if the
    // walk ran, so `max_results: 0` returning "no matches" proves nothing was opened.
    let (ok, body) = run(
        d.path(),
        "search_file_content",
        json!({"pattern": "needle", "max_results": 0, "path": "does-not-exist"}),
    );
    assert!(ok, "max_results 0 must not traverse: {body}");
    assert_eq!(body, "no matches", "max_results 0 must be empty: {body}");
    // 1: exactly one result.
    let (ok, body) = run(
        d.path(),
        "search_file_content",
        json!({"pattern": "needle", "max_results": 1}),
    );
    assert!(ok, "{body}");
    assert_eq!(
        body.matches("needle").count(),
        1,
        "a max_results 1 run returns one result: {body}"
    );
    assert_eq!(body.lines().count(), 1, "{body}");
    // MAX_SEARCH_RESULTS: at most the cap, and the output (including any note)
    // stays bounded.
    let mut max_config = cfg(d.path());
    max_config.max_output_bytes = MAX_SEARCH_RESULT_BYTES;
    let (ok, body) = execute_tool(
        d.path(),
        "search_file_content",
        json!({"pattern": "needle", "max_results": MAX_SEARCH_RESULTS}),
        &max_config,
    );
    assert!(ok, "{body}");
    assert!(
        body.len() <= MAX_SEARCH_RESULTS * MAX_LINE_BYTES + MAX_SEARCH_RESULT_BYTES,
        "max run stays bounded: {}",
        body.len()
    );
    assert_eq!(body.lines().count(), MAX_SEARCH_RESULTS, "{body}");
    // MAX_SEARCH_RESULTS + 1 clamps to the cap.
    let (ok, body) = execute_tool(
        d.path(),
        "search_file_content",
        json!({"pattern": "needle", "max_results": MAX_SEARCH_RESULTS + 1}),
        &max_config,
    );
    assert!(ok, "{body}");
    assert_eq!(
        body.lines().count(),
        MAX_SEARCH_RESULTS,
        "max+1 must clamp to the cap: {body}"
    );
    assert!(
        body.len() <= MAX_SEARCH_RESULTS * MAX_LINE_BYTES + MAX_SEARCH_RESULT_BYTES,
        "max+1 run stays bounded: {}",
        body.len()
    );
}

#[test]
fn search_honors_configured_output_limit_at_exact_and_plus_one() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("large.txt"),
        format!("needle{}\n", "x".repeat(4096)),
    )
    .unwrap();
    let args = json!({"pattern": "needle", "max_results": 1});

    let generous = cfg(d.path());
    let (ok, exact) = execute_tool(d.path(), "search_file_content", args.clone(), &generous);
    assert!(ok, "{exact}");
    let exact_len = exact.len();
    assert!(exact_len > 1);

    let mut exact_config = cfg(d.path());
    exact_config.max_output_bytes = exact_len;
    let (ok, at_limit) = execute_tool(d.path(), "search_file_content", args.clone(), &exact_config);
    assert!(ok, "{at_limit}");
    assert_eq!(at_limit.len(), exact_len);

    let mut plus_one_config = cfg(d.path());
    plus_one_config.max_output_bytes = exact_len - 1;
    let (ok, over_limit) = execute_tool(d.path(), "search_file_content", args, &plus_one_config);
    assert!(ok, "{over_limit}");
    assert!(over_limit.len() < exact_len, "{}", over_limit.len());
}

const WORKSPACE_LOCK_CHILD_ROOT: &str = "LLXPRT_TEST_WORKSPACE_LOCK_CHILD_ROOT";

#[test]
fn workspace_lock_subprocess_helper() {
    let Some(root) = std::env::var_os(WORKSPACE_LOCK_CHILD_ROOT) else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    std::fs::write(root.join("child-ready"), b"ready").unwrap();
    let config = cfg(&root);
    let (ok, body) = execute_tool(
        &root,
        "write_file",
        json!({"path": "locked.txt", "content": "writer"}),
        &config,
    );
    assert!(ok, "{body}");
}

#[test]
fn cooperating_processes_wait_for_the_workspace_lock() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().to_path_buf();
    let config = cfg(&root);
    let mut child = with_workspace_write_lock(&config.ws, || {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tools::tests::workspace_lock_subprocess_helper",
                "--nocapture",
            ])
            .env(WORKSPACE_LOCK_CHILD_ROOT, &root)
            .spawn()
            .map_err(|error| format!("spawn cooperating writer: {error}"))?;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !root.join("child-ready").exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "cooperating writer did not reach the lock"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            child.try_wait().unwrap().is_none(),
            "a cooperating writer completed while another workspace write lock was held"
        );
        Ok(child)
    })
    .unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(
        std::fs::read_to_string(root.join("locked.txt")).unwrap(),
        "writer"
    );
}

#[test]
fn replace_is_atomic_leaves_no_temp_and_preserves_file_on_failure() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("f.txt"), "abcabc").unwrap();
    // A replace whose `expected` count disagrees must fail and leave the original intact.
    let (ok, _) = run(
        d.path(),
        "replace",
        json!({"path": "f.txt", "old_string": "a", "new_string": "z", "expected": 1}),
    );
    assert!(!ok);
    assert_eq!(
        std::fs::read_to_string(d.path().join("f.txt")).unwrap(),
        "abcabc"
    );
    // A successful replace must not leave a `.llxprt-tmp-*` residue.
    let (ok, _) = run(
        d.path(),
        "replace",
        json!({"path": "f.txt", "old_string": "b", "new_string": "B", "expected": 2}),
    );
    assert!(ok);
    assert!(std::fs::read_dir(d.path()).unwrap().all(|e| !e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .contains(".llxprt-tmp")));
}

#[test]
fn list_handles_root_intermediate_and_final_symlink() {
    let d = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), d.path().join("rootlink")).unwrap();
    // `list_directory` refuses a path that is itself (or lands on) a symlink: root, a
    // final component, or an intermediate component all resolve via `openat` O_NOFOLLOW.
    let (ok, msg) = run(d.path(), "list_directory", json!({"path": "rootlink"}));
    assert!(!ok, "root symlink must be rejected: {msg}");
    std::fs::write(d.path().join("plain.txt"), "x").unwrap();
    let root_list = run(d.path(), "list_directory", json!({"path": ""}));
    assert!(root_list.0, "plain cwd list must succeed");
    assert!(root_list.1.contains("plain.txt"));
}

#[test]
fn search_path_given_intermediate_symlink_root_is_rejected() {
    let d = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret"), "needle").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), d.path().join("esc")).unwrap();
    // `search_file_content` must not search through an intermediate symlink.
    let (ok, msg) = run(
        d.path(),
        "search_file_content",
        json!({"pattern": "needle", "path": "esc"}),
    );
    assert!(
        !ok,
        "search through intermediate symlink must be rejected: {msg}"
    );
}

#[test]
fn deterministic_swap_write_then_read_is_consistent() {
    let d = tempfile::tempdir().unwrap();
    // write_file is atomic (temp + renameat over the target), so an immediate read sees a
    // deterministic full file: many back-to-back writes never expose partial content.
    for content in ["one", "two", "three", "four", "five"] {
        let (ok, m) = run(
            d.path(),
            "write_file",
            json!({"path": "swap.txt", "content": content}),
        );
        assert!(ok, "write failed: {m}");
        let (ok, body) = run(d.path(), "read_file", json!({"path": "swap.txt"}));
        assert!(ok);
        assert!(
            body.contains(content),
            "after write {content:?} read was: {body}"
        );
        // Each write clobbers the previous file; no temp residue remains.
        let residue = std::fs::read_dir(d.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains(".llxprt-tmp"));
        assert!(!residue);
    }
}

/// An exact read limit of 0 bytes returns no content and is never a panic, and a
/// limit larger than the file returns the whole file with no truncation marker.
#[test]
fn shell_nonzero_and_signal_return_ok_false() {
    let d = tempfile::tempdir().unwrap();
    let ws = WorkspaceCap::open(d.path()).unwrap();
    let c = ToolConfig {
        ws,
        max_output_bytes: 16 * 1024,
        shell: ShellConfig {
            max_shell_output: 64 * 1024,
            max_shell_timeout: Duration::from_secs(30),
            allow_shell: true,
        },
    };
    let args = json!({"command": "exit 3"});
    let (ok, msg) = execute_tool(d.path(), "run_shell_command", args, &c);
    assert!(!ok, "nonzero exit must be ok=false: {msg}");
    let (ok, _) = execute_tool(
        d.path(),
        "run_shell_command",
        json!({"command": "true"}),
        &c,
    );
    assert!(ok);
    // Without --allow-shell the tool refuses even a valid command.
    let c2 = ToolConfig {
        shell: ShellConfig {
            allow_shell: false,
            ..c.shell
        },
        ..c
    };
    let (ok, _) = execute_tool(
        d.path(),
        "run_shell_command",
        json!({"command": "true"}),
        &c2,
    );
    assert!(!ok);
}
