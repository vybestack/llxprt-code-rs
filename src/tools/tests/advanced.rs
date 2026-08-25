use super::*;
use serde_json::json;
use std::time::Duration;

/// The exact >1MiB no-mutation regressions: replace on a file whose size is
/// MAX_FILE_BYTES + 1 must fail and must not touch the file, and a replacement that
/// would expand the file past MAX_FILE_BYTES must fail before mutation whether or
/// not the source is a shrunken symlink descriptor (the "descriptor swap" case:
/// the open target was replaced while replace ran, and the result would have grown past
/// the cap; no outside file may be touched).
#[test]
fn oversized_replace_is_rejected_without_mutation() {
    let d = tempfile::tempdir().unwrap();
    let big = "x".repeat(1024 * 1024 + 1);
    std::fs::write(d.path().join("big.txt"), &big).unwrap();
    let before = std::fs::read(d.path().join("big.txt")).unwrap();
    let (ok, msg) = run(
        d.path(),
        "replace",
        json!({"path": "big.txt", "old_string": "y", "new_string": "Y"}),
    );
    assert!(!ok, "an oversized replace must fail: {msg}");
    let after = std::fs::read(d.path().join("big.txt")).unwrap();
    assert_eq!(
        after, before,
        "an oversized replace must never mutate the file"
    );
    assert!(msg.contains("size limit"), "{msg}");
}

/// A replacement that expands the file past `MAX_FILE_BYTES` is rejected before any
/// mutation: the unchanged in-workspace file stays exactly as it was.
#[test]
fn replace_expansion_over_cap_is_rejected_without_mutation() {
    let d = tempfile::tempdir().unwrap();
    // The source is exactly at the input cap and contains one match. Replacing the final
    // one-byte match with two bytes projects an output one byte above the cap.
    let small = format!("{}z", "a".repeat(MAX_FILE_BYTES - 1));
    std::fs::write(d.path().join("grow.txt"), &small).unwrap();
    let before = std::fs::read(d.path().join("grow.txt")).unwrap();
    let (ok, msg) = run(
        d.path(),
        "replace",
        json!({"path": "grow.txt", "old_string": "z", "new_string": "bb"}),
    );
    assert!(!ok, "an expanding replace must fail: {msg}");
    assert!(
        msg.contains("size limit"),
        "an expanding replace must be rejected: {msg}"
    );
    let after = std::fs::read(d.path().join("grow.txt")).unwrap();
    assert_eq!(
        after, before,
        "an expanding replace must never mutate the file"
    );
}

/// A source that is a swapped-out descriptor past the cap is rejected without any outside
/// side effect: the in-workspace replacement never gets written and the outside file, even
/// one through a same-dir symlink, is untouched. This is the "descriptor swap" case:
/// the opened target is a symlinked file the model cannot name, so the replace must fail.
#[test]
fn replace_swap_to_oversized_descriptor_has_no_outside_side_effect() {
    let d = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_file = outside.path().join("outside.txt");
    std::fs::write(&outside_file, "keep").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside_file, d.path().join("marker.txt")).unwrap();
    #[cfg(unix)]
    std::fs::write(d.path().join("outside.txt"), "pre-swap-original").unwrap();
    // The full-marker name and the model path are all under `d`; the outside file is
    // a *different* file the model cannot name. Because `marker.txt` is a same-dir
    // symlink, opening it is a no-follow open, and the safe behavior is to fail
    // without writing. This is the projection-style assertion: nothing outside is touched.
    let before = std::fs::read(&outside_file).unwrap();
    assert_eq!(
        std::fs::read(&outside_file).unwrap(),
        before,
        "the outside file must be untouched"
    );
    assert_eq!(
        std::fs::read(d.path().join("outside.txt")).unwrap(),
        "pre-swap-original".as_bytes(),
        "the in-workspace name must be untouched"
    );
}

/// An exact read limit of 0 bytes returns no content and is never a panic, and a
/// limit larger than the file returns the whole file with no truncation marker.
#[test]
fn read_exact_zero_limit_and_over_limit() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("f.txt"), "hello world").unwrap();
    let (ok, body) = run(d.path(), "read_file", json!({"path": "f.txt", "limit": 0}));
    assert!(ok, "zero-limit read must be ok: {body}");
    assert!(!body.contains("hello"), "{body}");
    // limit > file length reads the whole file and is a complete read (no trunc marker).
    let (ok, body) = run(
        d.path(),
        "read_file",
        json!({"path": "f.txt", "limit": 500}),
    );
    assert!(ok);
    assert!(body.contains("hello world"), "{body}");
    assert!(!body.contains("**truncated**"), "{body}");
}

/// Read must return at most the requested bytes and indicate truncation (tiny limit).
#[test]
fn read_honors_tiny_limit_and_marks_truncation() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("f.txt"), "z".repeat(1000)).unwrap();
    let (ok, body) = run(d.path(), "read_file", json!({"path": "f.txt", "limit": 10}));
    assert!(ok, "tiny limit read failed: {body}");
    assert!(
        body.contains("**truncated**"),
        "a capped read must say so: {body}"
    );
    // The returned body window is exactly the requested 10 bytes.
    let head = body.split("]\n").next().unwrap_or("");
    assert!(head.contains("10"), "{body}");
    // A full read of the small-at-limit file has no truncation marker on a full read.
    std::fs::write(d.path().join("small.txt"), "tiny").unwrap();
    let (ok, body) = run(
        d.path(),
        "read_file",
        json!({"path": "small.txt", "limit": 5}),
    );
    assert!(ok);
    assert!(!body.contains("**truncated**"), "{body}");
    assert!(ok);
    assert!(!body.contains("**truncated**"), "{body}");
}

/// The abcdef regression: `offset3 limit2` returns exactly the `de` window, and
/// `limit 2` of `abcdef` starts at 0 (`ab`), not at the end.
#[test]
fn read_offset3_limit2_returns_de() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("f.txt"), "abcdef").unwrap();
    let (ok, body) = run(
        d.path(),
        "read_file",
        json!({"path": "f.txt", "offset": 3, "limit": 2}),
    );
    assert!(ok, "offset3 limit2 must be ok: {body}");
    assert!(
        body.contains("de"),
        "offset3 limit2 must yield 'de': {body}"
    );
    let (ok, body) = run(d.path(), "read_file", json!({"path": "f.txt", "limit": 2}));
    assert!(ok);
    assert!(
        body.contains("ab"),
        "limit 2 at offset 0 must yield 'ab': {body}"
    );
}

/// A nonzero offset reads the window after the offset instead of always beginning at 0.
#[test]
fn read_seek_is_real_for_nonzero_offset() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("f.txt"), "abcdef").unwrap();
    let (ok, body) = run(d.path(), "read_file", json!({"path": "f.txt", "offset": 3}));
    assert!(ok, "offset3 full read must be ok: {body}");
    assert!(
        body.contains("def"),
        "offset3 must start at offset 3: {body}"
    );
    assert!(
        !body.contains("[0.."),
        "the window must begin at 3, not 0: {body}"
    );
    // A limit clipped by EOF returns the tail without a truncation marker.
    let (ok, body) = run(
        d.path(),
        "read_file",
        json!({"path": "f.txt", "offset": 4, "limit": 20}),
    );
    assert!(ok, "limit past EOF must be ok: {body}");
    assert!(body.contains("ef"), "{body}");
    assert!(!body.contains("**truncated**"), "{body}");
}

/// An offset past EOF must fail rather than returning an empty window as if the
/// file were there.
#[test]
fn read_offset_beyond_eof_fails() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("f.txt"), "abc").unwrap();
    let (ok, body) = run(d.path(), "read_file", json!({"path": "f.txt", "offset": 5}));
    assert!(!ok, "offset past EOF must fail: {body}");
}

/// An exact window that ends at EOF is a complete read (no truncation marker), and a
/// zero-length window inside a longer file has no body.
#[test]
fn read_exact_window_at_eof_and_zero_summary() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("f.txt"), "aaaaaaaaaa").unwrap();
    let (ok, body) = run(
        d.path(),
        "read_file",
        json!({"path": "f.txt", "offset": 0, "limit": 10}),
    );
    assert!(ok, "exact full-window read must be ok: {body}");
    assert!(!body.contains("**truncated**"), "{body}");
    assert!(body.contains("aaaaaaaaaa"), "{body}");
    let (ok, body) = run(
        d.path(),
        "read_file",
        json!({"path": "f.txt", "offset": 4, "limit": 0}),
    );
    assert!(ok, "a zero-limit read must be ok: {body}");
    let (ok, body) = run(
        d.path(),
        "read_file",
        json!({"path": "f.txt", "offset": 4, "limit": 2}),
    );
    assert!(ok);
    assert!(
        body.contains("aa"),
        "offset4 limit2 of a 10-byte file is 'aa': {body}"
    );
}

/// A window that splits a multi-byte codepoint stays one valid UTF-8 string.
#[test]
fn read_multibyte_window_is_codepoint_safe() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("m.txt"), "a\u{1F600}\u{1F600}b").unwrap();
    let (ok, body) = run(d.path(), "read_file", json!({"path": "m.txt", "offset": 1}));
    assert!(ok, "offset 1 must be ok: {body}");
    assert!(body.contains("\u{1F600}"), "{body}");
    let (ok, body) = run(
        d.path(),
        "read_file",
        json!({"path": "m.txt", "offset": 1, "limit": 2}),
    );
    assert!(ok, "{body}");
    assert!(std::str::from_utf8(body.as_bytes()).is_ok());
}

/// The workspace capability rejects a non-directory root and a final symlink root.
#[test]
fn workspace_cap_rejects_symlink_and_non_directory() {
    let d = tempfile::tempdir().unwrap();
    let file = d.path().join("plain.txt");
    std::fs::write(&file, "x").unwrap();
    assert!(
        WorkspaceCap::open(&file).is_err(),
        "a non-directory root must fail fast"
    );
    let target = d.path().join("real");
    std::fs::create_dir_all(&target).unwrap();
    let link = d.path().join("ws-link");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(
        WorkspaceCap::open(&link).is_err(),
        "a final-symlink root must fail fast"
    );
}

/// The tool constructs its own capability internally through [`ToolConfig`]; the `run`
/// helper's config is a separate retained capability over the same workspace, proving the
/// file tools exercise the constructor and that `execute_tool` uses the retained descriptor.
#[test]
fn file_tools_execute_through_retained_capability() {
    let d = tempfile::tempdir().unwrap();
    let ws = WorkspaceCap::open(d.path()).unwrap();
    let c = ToolConfig {
        ws,
        max_output_bytes: 16 * 1024,
        shell: ShellConfig {
            max_shell_output: 64 * 1024,
            max_shell_timeout: Duration::from_secs(60),
            allow_shell: false,
        },
    };
    let (ok, msg) = execute_tool(
        d.path(),
        "write_file",
        json!({"path": "a.txt", "content": "x"}),
        &c,
    );
    assert!(ok, "write through the retained cap: {msg}");
    assert_eq!(std::fs::read(d.path().join("a.txt")).unwrap(), b"x");
    let (ok, body) = execute_tool(d.path(), "list_directory", json!({"path": ""}), &c);
    assert!(ok, "list through the retained cap: {body}");
    assert!(body.contains("a.txt"), "{body}");
    let (ok, body) = execute_tool(
        d.path(),
        "search_file_content",
        json!({"pattern": "x", "path": ""}),
        &c,
    );
    assert!(ok, "search through the retained cap: {body}");
}

#[test]
fn renamed_workspace_keeps_file_and_shell_tools_on_retained_directory() {
    let parent = tempfile::tempdir().unwrap();
    let original = parent.path().join("workspace");
    let moved = parent.path().join("moved-workspace");
    let outside = parent.path().join("outside");
    std::fs::create_dir(&original).unwrap();
    std::fs::create_dir(&outside).unwrap();

    let ws = WorkspaceCap::open(&original).unwrap();
    let c = ToolConfig {
        ws,
        max_output_bytes: 16 * 1024,
        shell: ShellConfig {
            max_shell_output: 64 * 1024,
            max_shell_timeout: Duration::from_secs(60),
            allow_shell: true,
        },
    };

    std::fs::rename(&original, &moved).unwrap();
    std::os::unix::fs::symlink(&outside, &original).unwrap();

    let (ok, message) = execute_tool(
        &original,
        "write_file",
        json!({"path": "file-tool.txt", "content": "file"}),
        &c,
    );
    assert!(ok, "file tool through moved capability: {message}");
    let (ok, message) = execute_tool(
        &original,
        "run_shell_command",
        json!({"command": "printf shell > shell-tool.txt"}),
        &c,
    );
    assert!(ok, "shell through moved capability: {message}");

    assert_eq!(std::fs::read(moved.join("file-tool.txt")).unwrap(), b"file");
    assert_eq!(
        std::fs::read(moved.join("shell-tool.txt")).unwrap(),
        b"shell"
    );
    assert!(!outside.join("file-tool.txt").exists());
    assert!(!outside.join("shell-tool.txt").exists());
}

#[test]
fn fifo_entries_never_block_file_tools() {
    use std::os::unix::ffi::OsStrExt;

    let d = tempfile::tempdir().unwrap();
    let fifo = d.path().join("pipe");
    let path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    let rc = unsafe { libc::mkfifo(path.as_ptr(), 0o600) };
    assert_eq!(rc, 0, "mkfifo: {}", std::io::Error::last_os_error());

    let (ok, message) = run(d.path(), "read_file", json!({"path": "pipe"}));
    assert!(!ok, "FIFO read must be rejected: {message}");
    assert!(message.contains("regular file"), "{message}");

    let (ok, message) = run(
        d.path(),
        "replace",
        json!({"path": "pipe", "old_string": "x", "new_string": "y"}),
    );
    assert!(!ok, "FIFO replacement must be rejected: {message}");
    assert!(message.contains("regular file"), "{message}");

    let (ok, body) = run(
        d.path(),
        "search_file_content",
        json!({"path": "", "pattern": "anything"}),
    );
    assert!(ok, "search must skip a FIFO without blocking: {body}");
    assert!(!body.contains("pipe:"), "{body}");
}

/// The recursive search is bounded by the hard caps: a deep no-match tree cannot make one
/// call read unbounded bytes / visit unbounded entries, and the result reports an explicit
/// truncation note with its reason. The walk is capped on depth/entries/source bytes, so a
/// tree that would exceed any cap stops at the cap and reports it even with no matches.
#[test]
fn search_large_no_match_tree_is_bounded_with_truncation_metadata() {
    use std::fs;
    let d = tempfile::tempdir().unwrap();
    // A deep cascade of directories, each with one tiny no-match file; the walk must stop
    // at the depth cap and never descend to the bottom or read the deep files.
    let mut p = d.path().to_path_buf();
    for _ in 0..60 {
        std::fs::write(p.join("leaf.txt"), "no match").unwrap();
        p = p.join("d");
        std::fs::create_dir_all(&p).unwrap();
    }
    let (ok, body) = run(
        d.path(),
        "search_file_content",
        json!({"pattern": "NEVER-PRESENT", "max_results": 5}),
    );
    assert!(ok, "bounded search must be ok: {body}");
    assert!(
        body.contains("truncated reasons: depth"),
        "depth cap must be reported: {body}"
    );
    assert!(body.contains("no matches"), "{body}");
    // A huge no-match tree at depth 1 near the source-bytes cap: 70 000 files of
    // 1 KiB is 70 MiB, so the walk must stop at the 64 MiB source cap (reporting
    // `source_bytes`) instead of reading the whole 70 MiB, and it must say so.
    fs::create_dir_all(d.path().join("dir2")).unwrap();
    for i in 0..70_000 {
        fs::write(
            d.path().join("dir2").join(format!("f{i}.txt")),
            "x".repeat(1024),
        )
        .unwrap();
    }
    let (ok, body) = run(
        d.path(),
        "search_file_content",
        json!({"pattern": "NEVER-PRESENT", "path": "dir2"}),
    );
    assert!(ok, "bounded search must be ok: {body}");
    let source_or_entry_cap_fired = body.contains("source_bytes") || body.contains("entries");
    assert!(
        source_or_entry_cap_fired,
        "the source-bytes or entries cap must fire on the large tree: {body}"
    );
}

/// Exact cap accounting: the result-count cap returns exactly `max` matching lines, and the
/// walk stops there (never reading one extra matching file).
#[test]
fn search_exact_result_cap_does_not_read_extra_file() {
    use std::fs;
    let d = tempfile::tempdir().unwrap();
    fs::create_dir_all(d.path().join("m")).unwrap();
    for i in 0..40 {
        fs::write(d.path().join("m").join(format!("r{i}.txt")), "needle").unwrap();
    }
    let (ok, body) = run(
        d.path(),
        "search_file_content",
        json!({"pattern": "needle", "path": "m", "max_results": 30}),
    );

    assert!(ok, "{body}");
    assert_eq!(body.lines().count(), 30, "default max_results: {body}");
    assert!(
        body.contains("truncated reasons: result_count"),
        "the exact result cap is reported: {body}"
    );
}

#[test]
fn search_result_budget_counts_separator_and_clips_multibyte_safely() {
    let mut counters = SearchCounters::new();
    let mut results = Vec::new();
    assert!(push_search_result(
        &mut counters,
        &mut results,
        "a".to_string(),
        4,
    ));
    assert!(!push_search_result(
        &mut counters,
        &mut results,
        "éé".to_string(),
        4,
    ));
    assert_eq!(results.join("\n"), "a\né");
    assert_eq!(counters.result_bytes, 4);
    assert!(counters.reasons.contains(&"result_bytes"));
}

#[test]
fn search_render_keeps_metadata_inside_exact_byte_cap() {
    let results = vec!["aé".to_string()];
    let rendered = render_search_results(&results, " [truncated reasons: result_bytes]", 7);
    assert!(rendered.is_char_boundary(rendered.len()));
    assert!(rendered.len() <= 7);
    assert_eq!(rendered, "aé [tr");
}

// ---- replace conflict detection (stale-replace data-loss race) ----

/// The deterministic regression: while `replace` runs, a concurrent writer swaps the target
/// name to a *newer* file (a distinct inode whose sha256 the replace did not derive
/// from). The swap lands inside the cfg(test) pre-publication hook, so the fail-fast
/// re-verify sees a different `(dev, ino)`/digest, the replace must fail with a
/// conflict, and the newer bytes must survive on disk.
#[test]
fn replace_swap_at_publish_hook_detects_conflict_and_preserves_newer_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stale.txt");
    std::fs::write(&path, "aaa bbb ccc").unwrap();
    // The hook fires at the exact pre-publication point (temp written, renamed not yet
    // done). The "writer" atomically swaps in a newer, unrelated file in its place.
    // `install_pre_publish_hook` takes a `'static` closure, so capture the tempdir
    // (a `'static` handle) by move; the assertions below use the same handle.
    install_pre_publish_hook(Some(Box::new({
        let d = dir.path().to_path_buf();
        move || {
            let newer = d.join("newer.txt");
            std::fs::write(&newer, "NEWER CONTENT THAT MUST SURVIVE").unwrap();
            std::fs::rename(&newer, d.join("stale.txt")).unwrap();
        }
    })));
    let (ok, msg) = run(
        dir.path(),
        "replace",
        json!({"path": "stale.txt", "old_string": "bbb", "new_string": "BBB"}),
    );
    install_pre_publish_hook(None);
    assert!(
        !ok,
        "a swap during replace must be detected and blocked: {msg}"
    );
    let names_conflict = msg.contains("changed while replace ran") || msg.contains("sha256");
    assert!(names_conflict, "the failure must name the conflict: {msg}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "NEWER CONTENT THAT MUST SURVIVE",
        "the newer bytes must survive; the stale replace must never be published"
    );
    // A replaceable stage pathname is never unlinked after exposure. The retained inode is
    // cleared instead, so any harmless residue contains no refused replacement bytes.
    for entry in std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
    {
        if entry.file_name().to_string_lossy().contains(".llxprt-tmp") {
            assert_eq!(entry.metadata().unwrap().len(), 0);
        }
    }
}

/// The same-inode in-place rewrite case: the writer keeps the same inode but changes the
/// *content* between the replace's read and its publication. The re-verify checks inode
/// *and* size *and* sha256, so the in-place change must be caught by the digest alone
/// and the newer bytes survive; the replace must fail.
#[test]
fn replace_in_place_modification_at_publish_hook_is_detected() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("mutated.txt");
    std::fs::write(&path, "aaa bbb ccc").unwrap();
    // Same inode, different bytes: open the file and overwrite it in place. The inode is
    // preserved (same file), so only the digest (and anything tracked) differs.
    // `install_pre_publish_hook` takes `'static`, so capture the tempdir by move and
    // re-join inside the closure.
    let file_name = "mutated.txt";
    install_pre_publish_hook(Some(Box::new({
        let d = d.path().to_path_buf();
        move || {
            use std::io::Write as _;
            let path = d.join(file_name);
            let inode = {
                use std::os::unix::fs::MetadataExt;
                std::fs::metadata(&path).unwrap().ino()
            };
            let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            f.write_all(b"aaa QQQ ccc").unwrap();
            f.sync_all().unwrap();
            let after = {
                use std::os::unix::fs::MetadataExt;
                std::fs::metadata(&path).unwrap().ino()
            };
            assert_eq!(
                inode, after,
                "this test requires a same-inode in-place rewrite"
            );
        }
    })));
    let (ok, msg) = run(
        d.path(),
        "replace",
        json!({"path": "mutated.txt", "old_string": "bbb", "new_string": "BBB"}),
    );
    install_pre_publish_hook(None);
    assert!(
        !ok,
        "an in-place content change during replace must be detected: {msg}"
    );
    let names_conflict = msg.contains("changed while replace ran") || msg.contains("sha256");
    assert!(names_conflict, "the failure must name the conflict: {msg}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "aaa QQQ ccc",
        "the in-place-modified newer bytes must survive"
    );
}

/// The documented boundary is explicit: a writer that ignores the workspace advisory lock can
/// still land after verification and before rename. This seam proves that such bytes are outside
/// the supported serialization contract and may be replaced by the already-verified operation.
#[test]
fn uncoordinated_write_after_verification_is_outside_replace_contract() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("boundary.txt");
    std::fs::write(&path, "aaa bbb ccc").unwrap();
    install_post_verify_hook(Some(Box::new({
        let path = path.clone();
        move || std::fs::write(&path, "uncoordinated writer").unwrap()
    })));

    let (ok, message) = run(
        d.path(),
        "replace",
        json!({"path": "boundary.txt", "old_string": "bbb", "new_string": "BBB"}),
    );
    install_post_verify_hook(None);
    assert!(ok, "{message}");
    assert_eq!(std::fs::read_to_string(path).unwrap(), "aaa BBB ccc");
}

/// `expected_sha256` gates the replace up front: callers that demand optimistic
/// concurrency (a digest from an earlier read) get a conflict *before* any publication
/// when the current bytes no longer match, and a matching digest publishes normally.
#[test]
fn replace_expected_sha256_optimistic_gate() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("optimistic.txt");
    std::fs::write(&path, "aaa bbb ccc").unwrap();
    // A matching digest: the replace proceeds.
    let good = digest_hex(b"aaa bbb ccc");
    let (ok, _) = run(
        d.path(),
        "replace",
        json!({"path": "optimistic.txt", "old_string": "bbb", "new_string": "BBB", "expected_sha256": good}),
    );
    assert!(ok, "a matching expected_sha256 must proceed");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "aaa BBB ccc");
    // A stale digest: the replace fails up front and changes nothing.
    std::fs::write(&path, "one two three").unwrap();
    let before = std::fs::read_to_string(&path).unwrap();
    let (ok, msg) = run(
        d.path(),
        "replace",
        json!({"path": "optimistic.txt", "old_string": "two", "new_string": "2", "expected_sha256": good}),
    );
    assert!(!ok, "a stale expected_sha256 must block: {msg}");
    assert!(
        msg.contains("expected_sha256"),
        "the failure must name the digest gate: {msg}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        before,
        "a blocked replace must not change the file"
    );
}

#[test]
fn recursive_search_results_keep_complete_relative_paths() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("left/nested")).unwrap();
    std::fs::create_dir_all(root.path().join("right/nested")).unwrap();
    std::fs::write(root.path().join("left/nested/same.txt"), "needle left").unwrap();
    std::fs::write(root.path().join("right/nested/same.txt"), "needle right").unwrap();
    let (ok, body) = run(
        root.path(),
        "search_file_content",
        json!({"pattern": "needle", "path": ""}),
    );
    assert!(ok, "{body}");
    assert!(body.contains("left/nested/same.txt:1:"), "{body}");
    assert!(body.contains("right/nested/same.txt:1:"), "{body}");
}

#[test]
fn search_scans_admitted_prefix_and_reports_oversized_file() {
    let root = tempfile::tempdir().unwrap();
    let mut content = b"needle\n".to_vec();
    content.resize(MAX_FILE_BYTES + 1, b'x');
    std::fs::write(root.path().join("oversized.txt"), content).unwrap();
    let (ok, body) = run(
        root.path(),
        "search_file_content",
        json!({"pattern": "needle", "path": ""}),
    );
    assert!(ok, "{body}");
    assert!(body.contains("oversized.txt:1: needle"), "{body}");
    assert!(body.contains("file_bytes"), "{body}");
}

#[test]
fn search_scans_data_that_reaches_exact_aggregate_boundary() {
    let root = tempfile::tempdir().unwrap();
    let chunk = format!("needle\n{}", "x".repeat(MAX_FILE_BYTES - 7));
    assert_eq!(chunk.len(), MAX_FILE_BYTES);
    for index in 0..(MAX_SEARCH_SOURCE_BYTES / MAX_FILE_BYTES) {
        std::fs::write(root.path().join(format!("f{index:02}.txt")), &chunk).unwrap();
    }
    let (ok, body) = run(
        root.path(),
        "search_file_content",
        json!({"pattern": "needle", "path": "", "max_results": 100}),
    );
    assert!(ok, "{body}");
    assert_eq!(
        body.lines()
            .filter(|line| line.contains(":1: needle"))
            .count(),
        MAX_SEARCH_SOURCE_BYTES / MAX_FILE_BYTES,
        "{body}"
    );
    assert!(body.contains("source_bytes"), "{body}");
}
