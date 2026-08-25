//! Tool definitions and execution for the coding agent.
//!
//! Every workspace file tool path is confined to the session working directory through one
//! `openat` directory descriptor ([`WorkspaceCap`]) that the caller opens **once before
//! the first model request** and retains for the whole turn ([`ToolConfig::ws`]). All
//! component opens and the final target are opened descriptor-relative with `O_NOFOLLOW`, so
//! a concurrent path swap can never redirect an operation outside the workspace: the descriptor
//! is the boundary, and [`WorkspaceCap`] is **not** `Send`/`Sync`, so no tool
//! executes on any other thread's descriptor. The root is opened no-follow and verified to be
//! a real directory (a final symlink or a non-directory fails fast at open, never at a
//! later operation). The retained descriptor's `(dev, ino)` identity is checked on every
//! tool call. Renaming the directory does not redirect operations because they continue through
//! that descriptor; session reservation separately rejects a later pathname that resolves to a
//! different identity. File I/O never reopens or canonicalizes the cwd pathname.
//!
//! `write_file` and `replace` write atomically inside the workspace via a retained
//! same-directory staging descriptor plus `renameat`, so a reader never observes a partially
//! written file. Pre-publication failures clear the retained inode rather than unlinking an
//! attacker-replaceable name, so harmless zero-length staging entries can remain.
//! `list_directory` and `search_file_content` never follow symlinks and cap their
//! items/bytes. The agent `run_shell_command` runs with the **same** retained workspace
//! directory as cwd: the shell runner takes the fd of [`WorkspaceCap`] and `fchdir`s on
//! it at fork, so the shell and the file tools share one retained descriptor and neither
//! re-opens or canonicalizes the cwd pathname.
//!
//! `search_file_content` traversal itself is hard-capped: bounds on recursion depth, entries
//! visited, aggregate source bytes read, aggregate result bytes, and result count stop the walk
//! and surface an explicit truncation note with its reason, while keeping the current line-match
//! schema.
//!
//! Tool arguments are strictly typed per tool: missing required fields, wrong types, and extra
//! unknown fields all fail. `search_file_content` honors a bounded `max_results`,
//! `read_file` honors a bounded `limit`.

use serde_json::json;
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
mod publication;
mod replace;
mod search;

use publication::{atomic_write_into, atomic_write_into_after};
#[cfg(test)]
use publication::{
    fail_next_directory_sync, install_publication_hook, install_stage_substitution_hook,
    PublicationHookPoint,
};
use replace::replace_tool;
#[cfg(test)]
use replace::{digest_hex, install_post_verify_hook, install_pre_publish_hook};
use search::search_file_content_tool;
#[cfg(test)]
use search::{push_search_result, render_search_results, SearchCounters};

use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

/// Schema-property tuple: (name, json-schema, required).
pub type Property = (String, JsonValue, bool);

/// A tool surfaced to the model.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub properties: Vec<Property>,
}

/// The retained descriptor-relative workspace capability for the *file* tools.
///
/// Open it **once before the first model request** of a turn and move it into the
/// [`ToolConfig`]; every file-tool call in production executes through it and never re-opens or
/// canonicalizes the cwd pathname. It is deliberately neither `Send` nor `Sync`: the
/// structural `*mut ()` marker makes that a compiled guarantee, so no tool ever executes on
/// another thread's descriptor.
#[derive(Debug)]
pub struct WorkspaceCap {
    root: openat::Dir,
    dev: u64,
    ino: u64,
    /// Structural (non-`Send`/`Sync`) marker. `*mut ()` also suppresses the
    /// auto-`UnwindSafe`/`RefUnwindSafe` impls so the retained descriptor cannot be
    /// smuggled across threads even through a `catch_unwind` boundary; it exists only to
    /// opt us **out** of the auto traits, so there is no deref/access, no variance,
    /// and hence no soundness hole here.
    _not_send_sync: std::marker::PhantomData<*mut ()>,
}

/// Unix `open` flags for a **final entry** of a read/`replace`/search: a concrete,
/// non-`O_PATH` open so the returned descriptor is directly readable and its type can be
/// fstat'ed from the opened fd. `O_NOFOLLOW` rejects a final symlink; `O_NONBLOCK`
/// means a FIFO/device final entry opens without blocking.
const FILE_OPEN_FLAGS: libc::c_int =
    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;

/// The exact `WorkspaceCap` root open flags mandated by the fix: one atomic `open(2)` with
/// `O_RDONLY|O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC`. A directory open is a *directory*
/// on both Darwin and Linux; `O_NOFOLLOW` rejects a final-component symlink in the same
/// syscall as the open. The root is not `O_NONBLOCK` because a directory open can never
/// block.
const ROOT_OPEN_FLAGS: libc::c_int =
    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

/// Open and pin the workspace root from a path. Fails fast when the root does not exist,
/// is a final symlink (the open is no-follow), or is not a directory.
///
/// The root is opened with **one atomic `open(2)`** using
/// `O_RDONLY|O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC`, and the retained directory
/// capability is constructed from that exact descriptor (a `fstat` of the same fd, never a
/// stat of the name, never a to-be-followed name, never an `lstat`). No check-then-
/// open exists. `openat::Dir::open` is *not* trusted for the root: on Linux its base
/// flags are `O_PATH|O_CLOEXEC` (no `O_DIRECTORY`, no `O_NOFOLLOW`), so a
/// final-symlink root would **succeed** through it on Linux. Root symlinks must reject;
/// the raw `open(2)` with our own flags is what makes that atomic and no-follow.
///
/// `O_NONBLOCK` is deliberately absent: a `read_file` of a FIFO-like root is a *typed*
/// "not a regular file" failure, but the *root itself* must be a directory for the whole
/// turn, and a directory open never blocks. Only *final entries* get `O_NONBLOCK`.
pub fn open_root(root: &Path) -> Result<openat::Dir, String> {
    // The root path is a user-controlled pathname the tools honor; a raw NUL byte in
    // it is a clear fail with a fixed message (the NUL bytes never travel).
    let c = std::ffi::CString::new(root.as_os_str().as_bytes())
        .map_err(|_| "workspace root path contains a NUL byte".to_string())?;
    let fd = unsafe { libc::open(c.as_ptr(), ROOT_OPEN_FLAGS) };
    if fd < 0 {
        let e = std::io::Error::last_os_error();
        let raw = e.raw_os_error();
        // `O_NOFOLLOW` on a final symlink is `ELOOP` on Linux and `ENOTSUP` on
        // Darwin; a plain-file or non-directory name is `ENOTDIR` (and `EINVAL` on
        // Darwin). All of them are translated to the same model-visible wording as
        // before: the root failed to be a real directory.
        if raw == Some(libc::ELOOP)
            || raw == Some(libc::ENOTSUP)
            || raw == Some(libc::ENOTDIR)
            || raw == Some(libc::EINVAL)
        {
            return Err(format!(
                "workspace root is not a directory or is a symlink: {}",
                root.display()
            ));
        }
        if raw == Some(libc::ENOENT) {
            return Err(format!("workspace root does not exist: {}", root.display()));
        }
        return Err(format!(
            "workspace root cannot be opened: {}: {e}",
            root.display()
        ));
    }
    // SAFETY: `fd` was just returned nonnegative by `open`, so it is an owned
    // descriptor with nothing else touching it; `openat::Dir` now owns its close.
    //
    // SAFETY: `fd` was just returned nonnegative by `open`, so it is an owned
    // descriptor with nothing else touching it; `openat::Dir` now owns its close.
    // `openat::Dir` implements `FromRawFd` (in the `openat` crate); the `std`
    // negative-impl marker on [`WorkspaceCap`] keeps our raw descriptor pinned to this
    // thread, so the generated dir wrapper is confined exactly like the wrapper.
    let d = unsafe { <openat::Dir as std::os::unix::io::FromRawFd>::from_raw_fd(fd) };
    let m = d
        .self_metadata()
        .map_err(|e| format!("fstat workspace root: {e}"))?;
    if !m.is_dir() {
        return Err(format!(
            "workspace root is not a directory: {}",
            root.display()
        ));
    }
    Ok(d)
}

impl WorkspaceCap {
    /// Open and pin the workspace root from a path, record its `(dev, ino)` identity,
    /// and retain it for the whole turn. Fails fast when the root does not exist, is a
    /// final symlink, or is a non-directory.
    pub fn open(root: &Path) -> Result<WorkspaceCap, String> {
        let d = open_root(root)?;
        let m = d
            .self_metadata()
            .map_err(|e| format!("fstat workspace root: {e}"))?;
        let st = m.stat();
        let dev = u64::try_from(st.st_dev).unwrap_or(u64::MAX);
        let ino = st.st_ino;
        Ok(WorkspaceCap {
            root: d,
            dev,
            ino,
            _not_send_sync: std::marker::PhantomData,
        })
    }

    /// The `openat::Dir` handle of the retained workspace root. The file tools go through it
    /// for every path; the shell tool dup's its descriptor into the child so the shell
    /// executes relative to the **same** descriptor (a `fchdir` at fork, never a cwd
    /// pathname re-resolution or canonicalization).
    pub fn root_dir(&self) -> &openat::Dir {
        &self.root
    }

    /// Stable filesystem identity of the retained workspace directory.
    pub fn identity(&self) -> (u64, u64) {
        (self.dev, self.ino)
    }

    /// Duplicate the retained directory descriptor without resolving its pathname again.
    pub fn try_clone(&self) -> Result<WorkspaceCap, String> {
        let root = self
            .root
            .try_clone()
            .map_err(|error| format!("duplicate workspace root: {error}"))?;
        Ok(WorkspaceCap {
            root,
            dev: self.dev,
            ino: self.ino,
            _not_send_sync: std::marker::PhantomData,
        })
    }
}

/// The raw fd of the retained workspace root, handed to the shell runner. This is the
/// descriptor-relative cwd for agent shell execution.
fn shell_cwd_fd(cap: &WorkspaceCap) -> i32 {
    use std::os::unix::io::AsRawFd;
    cap.root_dir().as_raw_fd()
}

/// `(dev, ino)` of an open directory descriptor via a no-follow `fstat`. The dup is
/// checked **before** an owned `File` is constructed: when `dup`/`openat` returns
/// `-1` we return an error **without** wrapping `-1` in an owned `File` (which
/// would be undefined behavior). `dev`/`ino` come from `MetadataExt` on the opened
/// file and are `u64` on every Unix.
fn fd_identity(d: &openat::Dir) -> Result<(u64, u64), String> {
    use std::os::unix::io::AsRawFd;
    // SAFETY: `dup` on a valid open descriptor either duplicates it or returns `-1`;
    // the return is checked *before* any owned `File`/`RawFd` is constructed, so
    // the `-1` case never becomes an owned `File` (which would be UB).
    let fd = unsafe { libc::dup(d.as_raw_fd()) };
    if fd < 0 {
        return Err(format!(
            "dup workspace descriptor: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: `fd` was just returned nonnegative by `dup`, so it is an owned
    // descriptor with nothing else touching it; `File` now owns its close.
    let f = unsafe { <std::fs::File as std::os::unix::io::FromRawFd>::from_raw_fd(fd) };
    let m = f.metadata().map_err(|e| format!("fstat workspace: {e}"))?;
    use std::os::unix::fs::MetadataExt;
    Ok((m.dev(), m.ino()))
}
/// blocking on a (possible) FIFO: `openat` with `O_DIRECTORY|O_NOFOLLOW`. This is
/// used for **traversal** components, including a `search`/`list` start directory.
fn reopen_directory(dir: &openat::Dir) -> Result<openat::Dir, String> {
    use std::os::unix::io::{AsRawFd as _, FromRawFd as _};

    // SAFETY: the fixed `.` name is resolved relative to a valid retained directory descriptor.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            c".".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(format!(
            "re-open retained directory: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: `fd` is a fresh nonnegative descriptor, transferred exactly once.
    Ok(unsafe { openat::Dir::from_raw_fd(fd) })
}

fn open_named_dir(root: &openat::Dir, name: &str) -> Result<openat::Dir, String> {
    use std::os::unix::io::AsRawFd;
    if name == "." || name == ".." {
        return Err("'..'/'.' is rejected in a nested path".to_string());
    }
    let c = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| format!("path component {name:?} contains a NUL byte"))?;
    let fd = unsafe {
        libc::openat(
            root.as_raw_fd(),
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(format!(
            "open subdir {name}: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: `fd` was just returned nonnegative by `openat`; `openat::Dir` owns it.
    let d = unsafe { <openat::Dir as std::os::unix::io::FromRawFd>::from_raw_fd(fd) };
    let m = d
        .self_metadata()
        .map_err(|e| format!("fstat subdir {name}: {e}"))?;
    if !m.is_dir() {
        return Err(format!("{name} is not a directory"));
    }
    Ok(d)
}

/// Open a final entry inside `parent` without blocking on a FIFO/device, without following a
/// final symlink, and reject every non-regular opened object by descriptor metadata.
pub(crate) fn open_regular_os_at(
    parent: &openat::Dir,
    name: &std::ffi::OsStr,
) -> Result<std::fs::File, String> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::AsRawFd;

    let c = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| format!("path component {name:?} contains a NUL byte"))?;
    let fd = unsafe { libc::openat(parent.as_raw_fd(), c.as_ptr(), FILE_OPEN_FLAGS) };
    if fd < 0 {
        return Err(format!(
            "open {name:?}: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: `fd` was just returned nonnegative by `openat`; `File` owns its close.
    let file = unsafe { <std::fs::File as std::os::unix::io::FromRawFd>::from_raw_fd(fd) };
    if !file
        .metadata()
        .map_err(|error| format!("inspect {name:?}: {error}"))?
        .file_type()
        .is_file()
    {
        return Err(format!("{name:?} is not a regular file"));
    }
    Ok(file)
}

fn open_regular_at(parent: &openat::Dir, name: &str) -> Result<std::fs::File, String> {
    open_regular_os_at(parent, std::ffi::OsStr::new(name))
}

/// Static config that bounds tool behaviour.
///
/// The workspace capability ([`WorkspaceCap`]) is **retained for the entire turn** and is the
/// only boundary the file tools use. The shell derives its cwd descriptor directly from `ws`, so
/// file and shell tools cannot be configured with different workspace roots.
#[derive(Debug)]
pub struct ToolConfig {
    /// The retained descriptor that confines every file-tool path for the turn.
    pub ws: WorkspaceCap,
    pub max_output_bytes: usize,
    /// Per-tool shell bounds, kept separate from the retained file capability.
    pub shell: ShellConfig,
}

/// Per-call shell bounds. The agent shell executes with the retained workspace descriptor in
/// [`ToolConfig::ws`] as cwd.
#[derive(Debug)]
pub struct ShellConfig {
    pub max_shell_output: usize,
    /// Ceiling for any single shell command, independent of what the model asks for.
    pub max_shell_timeout: std::time::Duration,
    /// Whether `run_shell_command` is registered (`--allow-shell` gate).
    pub allow_shell: bool,
}

/// Serialize `write_file` and `replace` across every cooperating process that retained this
/// workspace directory. The lock is advisory: unrelated programs can still modify workspace
/// entries, so `replace` also performs its optimistic pre-publication identity/content check.
fn with_workspace_write_lock<T>(
    cap: &WorkspaceCap,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    use std::os::unix::io::AsRawFd;

    struct Guard(std::os::unix::io::RawFd);
    impl Drop for Guard {
        fn drop(&mut self) {
            // SAFETY: the retained workspace descriptor outlives this guard. Unlocking cannot
            // violate memory safety, and an unlock failure cannot be recovered during Drop.
            unsafe {
                libc::flock(self.0, libc::LOCK_UN);
            }
        }
    }

    let fd = cap.root_dir().as_raw_fd();
    loop {
        // SAFETY: `fd` is the valid retained workspace directory descriptor.
        if unsafe { libc::flock(fd, libc::LOCK_EX) } == 0 {
            break;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(format!("lock workspace for write: {error}"));
        }
    }
    let _guard = Guard(fd);
    operation()
}

/// Ceiling for `search_file_content` results and `read_file`/`list_directory` sizes.
const MAX_SEARCH_RESULTS: usize = 2000;
const DEFAULT_SEARCH_RESULTS: usize = 200;
const MAX_LIST_ITEMS: usize = 10_000;
/// Hard cap on how many bytes `read_file` ever allocates for one file, even if the caller
/// passes a huge `limit`; reading is bounded before any buffer is sized.
const MAX_FILE_BYTES: usize = 1024 * 1024;
/// Upper bound for one `search_file_content` result line, so a single match on an extremely
/// long line cannot push a result past the aggregate byte budget.
const MAX_LINE_BYTES: usize = 64 * 1024;
const MAX_LIST_BYTES: usize = MAX_LIST_ITEMS * 512;
/// Hard cap on `search_file_content` recursion depth (the walk stops at this depth even if
/// more directories exist below).
const MAX_SEARCH_DEPTH: usize = 32;
/// Hard cap on total directory entries visited by one search, including the start directory's
/// own entries.
const MAX_SEARCH_ENTRIES: usize = 200_000;
/// Hard cap on aggregate source bytes read across every searched file in one call; a
/// pathological (or adversarial) no-match tree can therefore never make one call read unbounded
/// bytes.
const MAX_SEARCH_SOURCE_BYTES: usize = 64 * 1024 * 1024;
/// Hard cap on aggregate serialized output bytes from one search.
const MAX_SEARCH_RESULT_BYTES: usize = MAX_SEARCH_RESULTS * MAX_LINE_BYTES;
/// Space reserved inside the result cap for all truncation-reason metadata.
const MAX_SEARCH_NOTE_BYTES: usize = 128;
const MAX_SEARCH_DATA_BYTES: usize = MAX_SEARCH_RESULT_BYTES - MAX_SEARCH_NOTE_BYTES;

/// The tool schemas sent with every model request.
/// Whether a name belongs to a tool that this agent can persist in a transcript.
pub(crate) fn is_known_tool_name(name: &str) -> bool {
    matches!(
        name,
        "read_file"
            | "write_file"
            | "replace"
            | "list_directory"
            | "search_file_content"
            | "run_shell_command"
    )
}

pub fn tool_specs(allow_shell: bool) -> Vec<ToolSpec> {
    let mut specs = vec![
        ToolSpec {
            name: "read_file".into(),
            description: "Read a file inside the project. Returns content (bounded and truncated on a character boundary) or an error.".into(),
            properties: vec![
                ("path".into(), json!({"type": "string"}), true),
                ("offset".into(), json!({"type": "integer"}), false),
                ("limit".into(), json!({"type": "integer"}), false),
            ],
        },
        ToolSpec {
            name: "write_file".into(),
            description: "Write content to a file inside the project, replacing any existing content.".into(),
            properties: vec![
                ("path".into(), json!({"type": "string"}), true),
                ("content".into(), json!({"type": "string"}), true),
            ],
        },
        ToolSpec {
            name: "replace".into(),
            description: "Replace a string in a file. Rejected unless the old string occurs exactly once (or the expected count is given). Concurrency: cooperating write_file and replace calls are serialized by an advisory lock on the retained workspace directory. After deriving the new bytes, replace also re-opens the target no-follow just before publishing and verifies the pathname still names the same unchanged content. If the inode, size, or a SHA-256 digest of the bytes it read differs, the replace fails with a conflict. `expected_sha256` (a lowercase hex SHA-256 of the current content, from a recent `read_file` digest) makes that same check an up-front requirement. The verify is not an atomic compare-and-swap: the re-open and rename are separate syscalls, and unrelated programs that do not honor the advisory lock can still change the name between them.".into(),
            properties: vec![
                ("path".into(), json!({"type": "string"}), true),
                ("old_string".into(), json!({"type": "string"}), true),
                ("new_string".into(), json!({"type": "string"}), true),
                ("expected".into(), json!({"type": "integer"}), false),
                ("expected_sha256".into(), json!({"type": "string"}), false),
            ],
        },
        ToolSpec {
            name: "list_directory".into(),
            description: "List files and subdirectories of a directory inside the project.".into(),
            properties: vec![("path".into(), json!({"type": "string"}), true)],
        },
        ToolSpec {
            name: "search_file_content".into(),
            description: "Search files inside the project for a regular-expression pattern, returning matching lines up to a bound. The recursive walk is hard-capped (depth, entries visited, aggregate source bytes, aggregate result bytes, and result count); when a cap is reached the walk stops at that point and the result carries an explicit truncation note with its reasons.".into(),
            properties: vec![
                ("pattern".into(), json!({"type": "string"}), true),
                ("max_results".into(), json!({"type": "integer"}), false),
                ("path".into(), json!({"type": "string"}), false),
            ],
        },
        ToolSpec {
            name: "run_shell_command".into(),
            description: "Run a command in the project root (only available with --allow-shell). Nonzero exit (or timeout) is reported as a failed result and should be fixed.".into(),
            properties: vec![
                ("command".into(), json!({"type": "string"}), true),
                ("timeout_seconds".into(), json!({"type": "integer"}), false),
            ],
        },
    ];
    specs.retain(|s| s.name != "run_shell_command" || allow_shell);
    specs
}

fn ws_root(ws: &WorkspaceCap) -> Result<&openat::Dir, String> {
    let (dev, ino) = fd_identity(&ws.root)?;
    if ws.dev != dev || ws.ino != ino {
        return Err("workspace identity changed: the workspace the capability was pinned to is gone (directory was renamed away, unmounted, or recreated)".to_string());
    }
    Ok(&ws.root)
}

/// Validate `rel` lexically: absolute paths, `..`/root components and `..` escapes are
/// rejected up front. Each existing component is lstat'ed via `openat` (never following a
/// symlink) so a symlink at any component is rejected.
fn resolve_comps(rel: &str) -> Result<Vec<String>, String> {
    let p = Path::new(rel);
    if p.is_absolute() {
        return Err(format!("absolute path rejected: {rel}"));
    }
    let mut comps = Vec::new();
    for c in p.components() {
        match c {
            Component::Normal(os) => {
                let s = os.to_string_lossy().into_owned();
                if s.is_empty() {
                    return Err(format!("path {rel:?} is not reachable: empty component"));
                }
                comps.push(s);
            }
            Component::ParentDir => return Err(format!("path uses '..' and is rejected: {rel}")),
            _ => {
                return Err(format!(
                    "path {rel:?} is not reachable under the project root"
                ));
            }
        }
    }
    Ok(comps)
}

/// Recursively create missing parents, each opened via `openat` so no component is followed
/// through a symlink. Returns the directory descriptor of the deepest parent.
fn ensure_parent_dir(root: &openat::Dir, comps: &[String]) -> Result<openat::Dir, String> {
    let mut cur = root
        .try_clone()
        .map_err(|e| format!("clone root dir: {e}"))?;
    for c in comps {
        if c.is_empty() || c == "." || c == ".." {
            return Err(format!("bad path component {c:?}"));
        }
        if cur.sub_dir(c).is_err() {
            cur.create_dir(c, 0o755)
                .map_err(|e| format!("mkdir {c}: {e}"))?;
        }
        cur = cur
            .sub_dir(c)
            .map_err(|e| format!("open subdir {c}: {e}"))?;
    }
    Ok(cur)
}

/// Open the deepest existing directory among `comps` (all `openat` with no-follow), never
/// creating anything for a read path.
fn ensure_parent_dir_read(root: &openat::Dir, comps: &[String]) -> Result<openat::Dir, String> {
    let mut cur = root
        .try_clone()
        .map_err(|e| format!("clone root dir: {e}"))?;
    for c in comps {
        cur = open_named_dir(&cur, c)?;
    }
    Ok(cur)
}

/// Truncate a string on a char boundary and never exceed `max` bytes **total including**
/// the marker; the marker's own bytes are reserved inside the budget, so no multi-byte
/// codepoint is split and the result stays within `max`.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let marker = format!("...  [truncated {} bytes]  ", s.len());
        if marker.len() >= max {
            // The marker alone cannot fit; keep at most `max` bytes of it.
            return marker[..max].to_string();
        }
        let budget = max - marker.len();
        let mut end = budget;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        let mut out: String = s[..end].to_string();
        out.push_str(&marker);
        out
    }
}

/// Reject unknown argument names.
fn reject_unknown(args: &BTreeMap<String, JsonValue>, known: &[&str]) -> Result<(), String> {
    for k in args.keys() {
        if !known.contains(&k.as_str()) {
            return Err(format!("unknown argument '{k}' for this tool"));
        }
    }
    Ok(())
}

/// Typed string arg; a missing required or a cached non-string is an error.
fn arg_str<'a>(
    args: &'a BTreeMap<String, JsonValue>,
    key: &str,
    required: bool,
) -> Result<Option<&'a str>, String> {
    match args.get(key) {
        None => {
            if required {
                Err(format!("missing required argument '{key}'"))
            } else {
                Ok(None)
            }
        }
        Some(JsonValue::String(s)) => Ok(Some(s)),
        Some(_) => Err(format!("argument '{key}' must be a string")),
    }
}

/// Typed non-negative integer arg.
fn arg_u64(args: &BTreeMap<String, JsonValue>, key: &str) -> Result<Option<u64>, String> {
    match args.get(key) {
        None => Ok(None),
        Some(JsonValue::Number(n)) => n
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("argument '{key}' must be a non-negative integer")),
        Some(_) => Err(format!("argument '{key}' must be an integer")),
    }
}

/// Bounded integer: default + ceiling, rejecting a value above the ceiling.
fn bounded(v: Option<u64>, default: usize, ceiling: usize) -> usize {
    v.map(|x| x.min(ceiling as u64) as usize).unwrap_or(default)
}

fn read_file_tool(
    cap: &WorkspaceCap,
    args: &BTreeMap<String, JsonValue>,
    max_output: usize,
) -> Result<String, String> {
    reject_unknown(args, &["path", "offset", "limit"])?;
    let rel = arg_str(args, "path", true)?.unwrap();
    let offset = arg_u64(args, "offset")?;
    let limit = arg_u64(args, "limit")?;
    let comps = resolve_comps(rel)?;
    if comps.is_empty() {
        return Err("path must name a file".into());
    }
    let (leaf_last, parent_comps) = comps.split_last().unwrap();
    let dir = ws_root(cap)?;
    let parent = ensure_parent_dir_read(dir, parent_comps)?;
    // The final entry is opened nonblocking/no-follow and its descriptor metadata is
    // checked: a regular file reads, everything else (FIFO, socket, device, directory)
    // is a typed error. A FIFO is opened with `O_NONBLOCK` and we return a typed
    // error without ever reading it (no blocking, no helper writer needed).
    let file = open_regular_at(&parent, leaf_last).map_err(|e| format!("open {leaf_last}: {e}"))?;
    let meta = file
        .metadata()
        .map_err(|e| format!("fstat {leaf_last}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{leaf_last} is not a regular file"));
    }
    let fd_len = meta.len();
    let offset = offset.unwrap_or(0);
    let fd_len_usize = usize::try_from(fd_len).unwrap_or(usize::MAX);
    if offset > fd_len {
        return Err(format!("offset {offset} is beyond the {fd_len} byte file"));
    }
    let offset = offset as usize;
    let mut file = file;
    if offset > 0 {
        file.seek(SeekFrom::Start(offset as u64))
            .map_err(|e| format!("seek {leaf_last}: {e}"))?;
    }
    // Read `cap + 1` when a limit is given so an exact-window request that reached the
    // end is never mistaken for a truncated read; the read stays bounded by the file size,
    // MAX_FILE_BYTES, and max_output.
    let max_window = usize::try_from(fd_len)
        .unwrap_or(usize::MAX)
        .min(MAX_FILE_BYTES)
        .min(max_output);
    let cap = match limit {
        Some(n) => usize::try_from(n)
            .unwrap_or(usize::MAX)
            .saturating_add(1)
            .min(max_window),
        None => max_window,
    };
    let data = drain_bytes(file, cap).map_err(|error| format!("read {rel}: {error}"))?;
    let shown = match limit {
        Some(n) => usize::try_from(n).unwrap_or(usize::MAX).min(data.len()),
        None => data.len(),
    };
    let end = offset + shown;
    let at_eof = offset + shown >= fd_len_usize;
    // The framed read is truncated as one value so the **total** model-visible string
    // (window header plus body) is at most `max_output`; a window that splits a
    // multi-byte codepoint is decoded lossily (still one valid String).
    let framed = if at_eof {
        format!(
            "[{offset}..{end} of {fd_len} bytes]\n{}",
            String::from_utf8_lossy(&data[..shown])
        )
    } else {
        // The file continues past the window: an exact `limit` window that stopped at
        // `limit` without reaching EOF is a truncated read, never a claim that the file
        // ends there.
        format!(
            "[{offset}..{end} of {fd_len} bytes **truncated**]\n{}",
            String::from_utf8_lossy(&data[..shown])
        )
    };
    Ok(truncate(&framed, max_output))
}

/// Read a reader's bytes up to `cap`: each read asks for no more than the remaining cap and
/// the loop never reads or retains more than `cap` bytes total. An I/O failure is never
/// confused with a successful EOF.
fn drain_bytes(mut reader: impl Read, cap: usize) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(cap.min(65536));
    let mut chunk = [0u8; 4096];
    while bytes.len() < cap {
        let want = (cap - bytes.len()).min(chunk.len());
        match reader.read(&mut chunk[..want]) {
            Ok(0) => break,
            Ok(count) => bytes.extend_from_slice(&chunk[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(bytes)
}

/// Write bytes atomically through [`WorkspaceCap`]: write to a same-dir temp file then
/// `renameat` it over the target, so no reader ever sees partial content.
fn atomic_write(cap: &WorkspaceCap, rel: &str, content: &[u8]) -> Result<PathBuf, String> {
    let comps = resolve_comps(rel)?;
    if comps.is_empty() {
        return Err("path must name a file".into());
    }
    if comps[0].is_empty() || comps[0] == "." || comps[0] == ".." {
        return Err(format!("bad final path component {:?}", comps[0]));
    }
    let (leaf_last, parent_comps) = comps.split_last().unwrap();
    let root = ws_root(cap)?;
    let parent = ensure_parent_dir(root, parent_comps)?;
    // Probe the final name via a no-follow lstat; a symlink at the final component is
    // rejected before the temp+rename touches it.
    if parent.read_link(leaf_last.as_str()).is_ok() {
        return Err(format!(
            "symlink rejected (no tool follows symlinks): {leaf_last}"
        ));
    }
    atomic_write_into(&parent, leaf_last, content)?;
    Ok(to_path_buf(leaf_last.as_str()))
}

fn to_path_buf(s: &str) -> PathBuf {
    PathBuf::from(s)
}

fn write_file_tool(
    cap: &WorkspaceCap,
    args: &BTreeMap<String, JsonValue>,
) -> Result<String, String> {
    reject_unknown(args, &["path", "content"])?;
    let rel = arg_str(args, "path", true)?.unwrap();
    let content = arg_str(args, "content", true)?.unwrap();
    let path = atomic_write(cap, rel, content.as_bytes())?;
    Ok(format!(
        "wrote {} bytes to {}",
        content.len(),
        path.display()
    ))
}

fn list_directory_tool(
    cap: &WorkspaceCap,
    args: &BTreeMap<String, JsonValue>,
    max_output_bytes: usize,
) -> Result<String, String> {
    reject_unknown(args, &["path"])?;
    let rel = arg_str(args, "path", true)?.unwrap();
    let dir = {
        let root = ws_root(cap)?;
        match rel {
            "" => reopen_directory(root)?,
            r => {
                let comps = resolve_comps(r)?;
                if comps.is_empty() {
                    reopen_directory(root)?
                } else {
                    let (leaf_last, parent_comps) = comps.split_last().unwrap();
                    if parent_comps.is_empty() {
                        open_named_dir(root, leaf_last)?
                    } else {
                        let parent = ensure_parent_dir_read(root, parent_comps)?;
                        open_named_dir(&parent, leaf_last)?
                    }
                }
            }
        }
    };
    let mut names = Vec::new();
    for e in dir
        .list_self()
        .map_err(|e| format!("read_dir {rel}: {e}"))?
    {
        let e = e.map_err(|e| format!("read_dir entry: {e}"))?;
        let kind = match e.simple_type() {
            Some(openat::SimpleType::Symlink) => "symlink",
            Some(openat::SimpleType::Dir) => "dir",
            _ => "file",
        };
        names.push(format!("{kind} {}", e.file_name().to_string_lossy()));
        if names.len() >= MAX_LIST_ITEMS {
            names.push("[truncated: too many entries]".into());
            break;
        }
    }
    let output_limit = MAX_LIST_BYTES.min(max_output_bytes);
    if names.is_empty() {
        Ok(truncate("(empty)", output_limit))
    } else {
        let mut out = String::new();
        for n in names {
            if out.len() >= output_limit {
                return Ok(truncate(
                    &format!("{out}\n[truncated: listing output too large]"),
                    output_limit,
                ));
            }
            // A single directory entry can be enormous; truncate on a char boundary so the
            // joined output stays inside the configured output limit including the marker.
            let n = truncate(&n, output_limit.saturating_sub(out.len()));
            out.push_str(&n);
            out.push('\n');
        }
        Ok(truncate(out.trim_end_matches('\n'), output_limit))
    }
}

/// Run a shell command via the shared bounded runner. Nonzero exit, a signal, or a timeout
/// is an `Err` carrying the captured output (the model sees `ok=false`).
fn shell_tool(
    fd: i32,
    args: &BTreeMap<String, JsonValue>,
    max_timeout: std::time::Duration,
    max_output: usize,
) -> Result<String, String> {
    reject_unknown(args, &["command", "timeout_seconds"])?;
    let command = arg_str(args, "command", true)?.unwrap();
    if command.trim().is_empty() {
        return Err("command must not be empty".into());
    }
    let timeout = bounded(
        arg_u64(args, "timeout_seconds")?,
        max_timeout.as_secs() as usize,
        max_timeout.as_secs() as usize,
    );
    let timeout = std::time::Duration::from_secs(u64::from(
        u32::try_from(timeout.max(1)).unwrap_or(u32::MAX),
    ));
    let o = crate::process::run_cmd(crate::process::CmdSpec {
        program: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), command.to_string()],
        cwd: None,
        cwd_fd: Some(fd),
        env_add: Vec::new(),
        timeout,
        max_output,
    })?;
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    // Every model-visible shell string (success or failure diagnostic) is bounded as one
    // value, framing and combined output included, to `max_output`.
    let s = if o.timed_out {
        format!(
            "command timed out after {} ms; output:\n{}",
            timeout.as_millis(),
            combined.trim_end()
        )
    } else {
        match o.status {
            Some(0) => combined.trim_end().to_string(),
            Some(code) => format!(
                "command exited with {code}; output:\n{}",
                combined.trim_end()
            ),
            None => format!(
                "command was killed by a signal; output:\n{}",
                combined.trim_end()
            ),
        }
    };
    let bounded = truncate(&s, max_output);
    if o.timed_out {
        Err(bounded)
    } else {
        match o.status {
            Some(0) => Ok(bounded),
            Some(_) | None => Err(bounded),
        }
    }
}

/// Choose and execute a tool by name, returning `(ok, text)`. `ok=false` is a normal,
/// model-visible tool result. All five *file* tools execute through the retained
/// [`WorkspaceCap`] on [`ToolConfig::ws`]; `run_shell_command` derives its cwd descriptor from
/// that same capability, so file and shell tools share one retained workspace and the shell never
/// re-resolves or canonicalizes the cwd pathname.
pub fn execute_tool(
    root: &Path,
    name: &str,
    args: JsonValue,
    config: &ToolConfig,
) -> (bool, String) {
    execute_tool_with_limit(root, name, args, config, config.max_output_bytes)
}

/// Execute one tool without allowing its rendered result to exceed the caller's remaining
/// aggregate output budget.
pub(crate) fn execute_tool_with_limit(
    root: &Path,
    name: &str,
    args: JsonValue,
    config: &ToolConfig,
    remaining_output_bytes: usize,
) -> (bool, String) {
    let _ = root;
    let output_limit = config.max_output_bytes.min(remaining_output_bytes);
    let map = match args {
        JsonValue::Object(m) => m.into_iter().collect::<BTreeMap<_, _>>(),
        _ => {
            return (
                false,
                truncate("tool arguments must be a JSON object", output_limit),
            )
        }
    };
    let result = match name {
        "read_file" => read_file_tool(&config.ws, &map, output_limit),
        "write_file" => with_workspace_write_lock(&config.ws, || write_file_tool(&config.ws, &map)),
        "replace" => with_workspace_write_lock(&config.ws, || replace_tool(&config.ws, &map)),
        "list_directory" => list_directory_tool(&config.ws, &map, output_limit),
        "search_file_content" => search_file_content_tool(&config.ws, &map, output_limit),
        "run_shell_command" => {
            if !config.shell.allow_shell {
                Err("run_shell_command is disabled; enable it with --allow-shell".into())
            } else {
                shell_tool(
                    crate::tools::shell_cwd_fd(&config.ws),
                    &map,
                    config.shell.max_shell_timeout,
                    config.shell.max_shell_output.min(output_limit),
                )
            }
        }
        _ => Err(format!("unknown tool {name}")),
    };
    match result {
        Ok(s) => (true, truncate(&s, output_limit)),
        Err(e) => (false, truncate(&e, output_limit)),
    }
}
#[cfg(test)]
mod tests;
