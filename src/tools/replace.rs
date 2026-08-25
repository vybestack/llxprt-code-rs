use super::*;

/// An optimistic-concurrency conflict found by the pre-publication re-verify. The newer
/// content is left in place so the caller can resolve it by re-reading and retrying.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReplaceConflict {
    /// The human message a tool surfaces for the conflict.
    reason: String,
}

/// Re-open the **final name** (`leaf` under `parent`) descriptor-relative and no-follow, and
/// verify it still names the same unchanged inode, type, size, and SHA-256 digest that
/// `expected` records.
///
/// This is the fail-fast check between "derived bytes ready" and "bytes published". The
/// re-open runs **after** the temp bytes are written and synced and **before** the rename, so
/// it detects a concurrent rename/swap of the name (`dev`/`ino`) *and* an in-place
/// content change on the same inode (digest) that landed before the re-open. The check is *not*
/// an atomic compare-and-swap: the re-open/verify and rename are separate syscalls. The workspace
/// advisory lock serializes cooperating `write_file` and `replace` calls across processes, while
/// an unrelated writer can still change the name in the remaining window. This boundary is
/// documented in the `replace` tool schema.
fn verify_publish_unchanged(
    parent: &openat::Dir,
    leaf: &str,
    expected: &PublishCheckpoint,
) -> Result<(), ReplaceConflict> {
    use std::os::unix::fs::MetadataExt;

    let conflict = |detail: &str| {
        ReplaceConflict {
        reason: format!(
            "replace blocked: {leaf} changed while replace ran ({detail}); newer content preserved; re-read and retry the replace"
        ),
    }
    };
    let now = open_regular_at(parent, leaf)
        .map_err(|_| conflict("the final name could not be reopened as a regular file"))?;
    let meta = now
        .metadata()
        .map_err(|_| conflict("the final descriptor could not be inspected"))?;
    if !meta.is_file()
        || meta.len() != expected.len
        || meta.dev() != expected.dev
        || meta.ino() != expected.ino
    {
        return Err(conflict("the final file identity, type, or size differs"));
    }
    let bytes = drain_bytes(now, MAX_FILE_BYTES + 1)
        .map_err(|_| conflict("the final file could not be reread"))?;
    let now_digest = digest_hex(&bytes);
    if now_digest != expected.sha256 {
        return Err(conflict("the final file content changed in place"));
    }
    Ok(())
}

/// The `(dev, ino, len, sha256)` the replace derivation read, against which the final name
/// is re-verified immediately before publication.
struct PublishCheckpoint {
    dev: u64,
    ino: u64,
    len: u64,
    sha256: String,
}

/// Lowercase hex SHA-256 of `data`: the strong digest `replace` uses to detect that a final
/// name's bytes differ from the ones its derived output was computed from.
pub(super) fn digest_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

// A deterministic, test-only seam invoked immediately before an atomic tool publish (the rename of
// derived bytes over a final name). Set a callback before the publish a test wants to race
// with: the callback runs at the exact pre-publication point (after the temp bytes are written
// and synced, before the rename), so a test can swap in newer content or rewrite the same
// inode and assert the publish then fails without clobbering it. The seam fires once per
// set callback and is thread-local, so parallel tests cannot cross-trigger one another.
#[cfg(test)]
thread_local! {
    static PRE_PUBLISH_HOOK: std::cell::RefCell<Option<Box<dyn Fn()>>> = const {
        std::cell::RefCell::new(None)
    };
}

/// Install the test-only pre-publication callback (or `None` to clear it).
#[cfg(test)]
pub fn install_pre_publish_hook(cb: Option<Box<dyn Fn()>>) {
    PRE_PUBLISH_HOOK.with(|h| h.replace(cb));
}

/// Run the installed test-only pre-publication hook once, if any.
#[cfg(test)]
fn run_pre_publish_hook() {
    PRE_PUBLISH_HOOK.with(|h| {
        if let Some(cb) = h.borrow_mut().take() {
            cb();
        }
    });
}

// Test-only seam after the optimistic verification and immediately before rename. This models an
// uncoordinated writer that does not honor the workspace advisory lock.
#[cfg(test)]
thread_local! {
    static POST_VERIFY_HOOK: std::cell::RefCell<Option<Box<dyn Fn()>>> = const {
        std::cell::RefCell::new(None)
    };
}

#[cfg(test)]
pub fn install_post_verify_hook(cb: Option<Box<dyn Fn()>>) {
    POST_VERIFY_HOOK.with(|hook| hook.replace(cb));
}

#[cfg(test)]
fn run_post_verify_hook() {
    POST_VERIFY_HOOK.with(|hook| {
        if let Some(callback) = hook.borrow_mut().take() {
            callback();
        }
    });
}

/// Publish derived bytes only after the original target still matches its checkpoint. The shared
/// publication path retains and authenticates the staging descriptor, verifies the installed
/// digest, syncs the parent directory, and verifies the installed file again.
fn publish_derived_bytes(
    parent: &openat::Dir,
    leaf: &str,
    content: &[u8],
    expected: &PublishCheckpoint,
) -> Result<(), ReplaceConflict> {
    atomic_write_into_after(parent, leaf, content, || {
        #[cfg(test)]
        run_pre_publish_hook();
        verify_publish_unchanged(parent, leaf, expected).map_err(|conflict| conflict.reason)?;
        #[cfg(test)]
        run_post_verify_hook();
        Ok(())
    })
    .map_err(|reason| ReplaceConflict { reason })
}

/// The `replace`-specific entry point that [`replace_tool`] uses for publication. It is the
/// same temp-file-then-rename publish as [`publish_derived_bytes`], guarded by the
/// pre-publication re-verify; the distinct name keeps `write_file`'s atomic path (which
/// intentionally has no pre-check) separate.
fn replace_publish_atomic(
    parent: &openat::Dir,
    leaf: &str,
    content: &[u8],
    expected: &PublishCheckpoint,
) -> Result<(), ReplaceConflict> {
    publish_derived_bytes(parent, leaf, content, expected)
}

struct ReplaceSource {
    parent: openat::Dir,
    leaf: String,
    display: PathBuf,
    source: String,
    checkpoint: PublishCheckpoint,
}

fn load_replace_source(cap: &WorkspaceCap, rel: &str) -> Result<ReplaceSource, String> {
    let comps = resolve_comps(rel)?;
    let (leaf, parent_comps) = comps
        .split_last()
        .ok_or_else(|| "path must name a file".to_string())?;
    let parent = ensure_parent_dir_read(ws_root(cap)?, parent_comps)?;
    let display = to_path_buf(leaf.as_str());
    let file = open_regular_at(&parent, leaf)
        .map_err(|error| format!("open {}: {error}", display.display()))?;
    let meta = file
        .metadata()
        .map_err(|error| format!("fstat {}: {error}", display.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", display.display()));
    }
    if meta.len() > MAX_FILE_BYTES as u64 {
        return Err(replace_size_error(&display));
    }
    let data = drain_bytes(file, MAX_FILE_BYTES + 1)
        .map_err(|error| format!("read {}: {error}", display.display()))?;
    if data.len() > MAX_FILE_BYTES {
        return Err(replace_size_error(&display));
    }
    if data.len() as u64 != meta.len() {
        return Err(format!(
            "{} changed while replace read it; re-read and retry",
            display.display()
        ));
    }
    let source = String::from_utf8(data.clone())
        .map_err(|_| format!("{} is not valid UTF-8", display.display()))?;
    use std::os::unix::fs::MetadataExt;
    let checkpoint = PublishCheckpoint {
        dev: meta.dev(),
        ino: meta.ino(),
        len: meta.len(),
        sha256: digest_hex(&data),
    };
    Ok(ReplaceSource {
        parent,
        leaf: leaf.clone(),
        display,
        source,
        checkpoint,
    })
}

fn replace_size_error(path: &Path) -> String {
    format!(
        "{} is above the supported replace size limit; no change made",
        path.display()
    )
}

pub(super) fn replace_tool(
    cap: &WorkspaceCap,
    args: &BTreeMap<String, JsonValue>,
) -> Result<String, String> {
    reject_unknown(
        args,
        &[
            "path",
            "old_string",
            "new_string",
            "expected",
            "expected_sha256",
        ],
    )?;
    let rel = arg_str(args, "path", true)?.unwrap();
    let old = arg_str(args, "old_string", true)?.unwrap();
    let new = arg_str(args, "new_string", true)?.unwrap();
    let expected = arg_u64(args, "expected")?;
    let expected_sha256 = arg_str(args, "expected_sha256", false)?;
    if old.is_empty() {
        return Err("old_string must not be empty".into());
    }
    let source_file = load_replace_source(cap, rel)?;
    let source = &source_file.source;
    let rela_leaf = &source_file.display;
    let count = source.matches(old).count();
    let replace_count = match expected {
        Some(expected) if count == expected as usize => expected,
        Some(expected) => {
            return Err(format!(
                "expected {expected} occurrence(s) of {old:?} but found {count} in {}; no change made",
                rela_leaf.display()
            ));
        }
        None if count == 1 => 1,
        None => {
            return Err(format!(
                "expected exactly one occurrence of {old:?} but found {count} in {}; no change made",
                rela_leaf.display()
            ));
        }
    };
    let projected = projected_replace_len(source.len(), old.len(), new.len(), replace_count)
        .ok_or_else(|| {
            "replacement output would exceed the supported replace size limit; no change made"
                .to_string()
        })?;
    if projected > MAX_FILE_BYTES {
        return Err(
            "replacement output would exceed the supported replace size limit; no change made"
                .into(),
        );
    }
    let updated = replace_n(source, old, new, replace_count);
    // Optimistic-concurrency gate: when the caller supplies `expected_sha256`, it must match
    // the exact bytes the replacement was derived from (the caller's precondition, e.g. from a
    // prior read), or the replace fails before writing anything.
    if let Some(want) = expected_sha256 {
        if want != source_file.checkpoint.sha256 {
            return Err(format!(
                "replace blocked: expected_sha256 {want} does not match current content (sha256 {}) in {}; no change made; re-read and retry",
                &source_file.checkpoint.sha256[..16],
                rela_leaf.display()
            ));
        }
    }
    replace_publish_atomic(
        &source_file.parent,
        &source_file.leaf,
        updated.as_bytes(),
        &source_file.checkpoint,
    )
    .map_err(|conflict| conflict.reason)?;
    Ok(format!(
        "replaced {count} occurrence(s) in {}",
        source_file.leaf
    ))
}

fn projected_replace_len(source: usize, old: usize, new: usize, count: u64) -> Option<usize> {
    let count = usize::try_from(count).ok()?;
    if new >= old {
        source.checked_add(new.checked_sub(old)?.checked_mul(count)?)
    } else {
        source.checked_sub(old.checked_sub(new)?.checked_mul(count)?)
    }
}

fn replace_n(hay: &str, needle: &str, repl: &str, n: u64) -> String {
    if n == 0 || needle.is_empty() {
        return hay.to_string();
    }
    let mut out = String::with_capacity(hay.len());
    let mut remaining = n;
    let mut rest = hay;
    while remaining > 0 {
        if let Some(idx) = rest.find(needle) {
            out.push_str(&rest[..idx]);
            out.push_str(repl);
            rest = &rest[idx + needle.len()..];
            remaining -= 1;
        } else {
            out.push_str(rest);
            return out;
        }
    }
    out.push_str(rest);
    out
}
