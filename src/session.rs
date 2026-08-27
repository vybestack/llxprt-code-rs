//! Versioned session storage for the headless agent.
//!
//! Two generation-numbered state slots (`session.json` and `session.alt.json`) provide
//! crash recovery without publishing through a replaceable temporary pathname. The newest valid
//! slot holds `session_id`, the canonical pinned `cwd`, and an explicit list of `branches`. Each
//! branch carries its own `branch_id`, parent lineage
//! (parent `branch_id` + turn + attempt), a 1-based `turn`, an `attempt` id, the
//! exact prompt and its FNV-1a digest, the owner token + lease timestamps of its
//! reservation, a `lifecycle` enum (`pending`/`completed`/`failed`), every
//! assistant response as `rounds`, each tool call's id/name/raw args, each tool
//! result, and the final `summary`/`error`. Prompts are capped at
//! [`MAX_PROMPT_BYTES`] (the input limit) before anything is persisted; the session
//! id is a bounded single path component.

use fs2::FileExt;
use std::io::Read as _;

/// The one supported on-disk format.
pub const STORE_VERSION: u32 = 2;
/// Bounded lease for a reservation, in seconds. A `pending` branch whose
/// `lease_expiry` is in the past may be reclaimed by another process.
pub const LEASE_SECONDS: u64 = 3600;
/// Upper bound on one prompt (bytes). A prompt over this is rejected up front so the
/// persisted transcript and the model request stay bounded.
pub const MAX_PROMPT_BYTES: usize = 512 * 1024;
/// Hard cap on one state slot (bytes). Each slot is read bounded (`cap + 1`) so an
/// oversized state file is rejected before any allocation, and every write checks the serialized
/// size before modifying the inactive slot.
pub const MAX_SESSION_BYTES: usize = 32 * 1024 * 1024;
/// Hard cap on the number of branches in one session.
pub const MAX_BRANCHES: usize = 4096;
/// Hard cap on the number of tool entries persisted for one branch.
pub const MAX_TOOL_ENTRIES: usize = 20_000;
/// Maximum time spent waiting for another process or thread to release a session lock.
const SESSION_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// Poll interval while waiting for a contended session lock.
const SESSION_LOCK_RETRY: std::time::Duration = std::time::Duration::from_millis(10);

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// One round of the turn loop: the assistant message (with its tool calls) and the
/// executed results. A final no-tool round has empty `calls`; this is the
/// assistant's final response and is always persisted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoundRecord {
    /// Assistant text emitted alongside the tool calls of this round.
    pub assistant: String,
    /// The tool calls made in this round (empty for the final response).
    pub calls: Vec<ToolCallRecord>,
}

/// A recorded tool call for a persisted tool transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    /// The provider-assigned tool call id (persisted and replayed verbatim).
    pub id: String,
    pub name: String,
    /// Valid argument-object JSON in the adapter's semantic representation. Providers preserve
    /// malformed raw strings only long enough for the host to reject them before execution.
    pub args: String,
    /// Whether the tool run reported success.
    pub ok: bool,
    pub result: String,
}

/// Lifecycle of one branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Pending,
    Completed,
    Failed,
}

/// A single addressable attempt at one turn. `parent_*` records the lineage this
/// branch continues: the branch it was forked from, plus that parent's turn and attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchRecord {
    /// Globally unique branch id, allocated from a checked counter.
    pub branch_id: String,
    /// 1-based turn number within the session.
    pub turn: u32,
    /// Attempt id within this turn (1 for the first attempt, 2+ for branches).
    pub attempt: u32,
    /// The branch this one continues (its parent lineage). `None` for the root branch.
    pub parent_branch: Option<String>,
    pub parent_turn: u32,
    pub parent_attempt: u32,
    /// The exact prompt that started this attempt.
    pub prompt: String,
    pub digest: String,
    /// Lifecycle state of this branch.
    pub lifecycle: Lifecycle,
    /// The complete round history, including the final no-tool response.
    #[serde(default)]
    pub rounds: Vec<RoundRecord>,
    /// Final plain-text summary (completed branches).
    #[serde(default)]
    pub summary: String,
    /// Terminal error description (failed branches).
    #[serde(default)]
    pub error: String,
    /// Unique owner token of the process holding the reservation.
    #[serde(default)]
    pub owner: String,
    /// Wall-clock unix seconds when the reservation was made.
    #[serde(default)]
    pub reserved_at: u64,
    /// Wall-clock unix seconds when the lease expires; past means stale.
    #[serde(default)]
    pub lease_expiry: u64,
}

/// The versioned persistent payload stored inside each state slot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    pub version: u32,
    pub session_id: String,
    /// Canonical working directory pinned on the first turn.
    pub cwd: Option<String>,
    /// Filesystem device and inode of the atomically opened workspace root.
    #[serde(default)]
    pub cwd_dev: u64,
    #[serde(default)]
    pub cwd_ino: u64,
    #[serde(default)]
    pub branches: Vec<BranchRecord>,
    /// Source of unique branch ids (checked increments).
    #[serde(default)]
    pub next_branch_seq: u64,
}

impl SessionState {
    fn empty(session_id: &str) -> SessionState {
        SessionState {
            version: STORE_VERSION,
            session_id: session_id.to_string(),
            cwd: None,
            cwd_dev: 0,
            cwd_ino: 0,
            branches: Vec::new(),
            next_branch_seq: 0,
        }
    }
}

mod reserve;
mod validate;

/// A turn's history materialized for a model request: the completed turn's prompt, full
/// round history (including the final assistant response), and summary.
#[derive(Debug, Clone)]
pub struct HistoryTurn {
    pub turn: u32,
    pub attempt: u32,
    pub branch_id: String,
    pub prompt: String,
    pub rounds: Vec<RoundRecord>,
    pub summary: String,
}

struct RequestLease {
    owner: String,
    now: u64,
    lease_end: u64,
}

/// The outcome of [`SessionStore::start_request`].
#[derive(Debug, Clone)]
pub struct ReservedRequest {
    /// The reserved (or replayed) branch.
    pub branch_id: String,
    pub turn: u32,
    pub attempt: u32,
    /// True when this is a network-free replay of an already-**completed** branch.
    pub replay: bool,
    /// True when this is a fresh retry attempt of a previously **failed** prompt at the
    /// same turn. A retry is never a replay and never reports ok on its own.
    pub retry: bool,
    /// The completed branch's rounds (populated when `replay`).
    pub rounds: Vec<RoundRecord>,
    pub summary: String,
    pub prompt: String,
    /// Prior-turn history materialized for this branch (empty when `replay`). Sibling
    /// branches are never mixed in.
    pub history: Vec<HistoryTurn>,
    /// The unique owner token of this reservation.
    pub owner: String,
}

/// Session id: a validated, bounded identifier (a single safe path component), so
/// `--session` cannot escape the sessions root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionId {
    pub id: String,
}

/// Errors from the session store.
pub enum StoreError {
    Invalid(String),
    Io(String),
    Corrupt(String),
    Stale,
    Busy(String),
    Lock(String),
    LockTimeout,
    /// A state-slot update completed, but retained-directory durability was not confirmed.
    InstalledDurabilityUnknown,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let rendered = match self {
            StoreError::Invalid(m) => m.clone(),
            StoreError::Io(m) => format!("session io: {m}"),
            StoreError::Corrupt(m) => format!("corrupt session state: {m}"),
            StoreError::Stale => "session state changed since reservation; retry".to_string(),
            StoreError::Busy(m) => format!("session reservation active: {m}"),
            StoreError::Lock(m) => format!("session lock: {m}"),
            StoreError::LockTimeout => "session lock timed out; retry".to_string(),
            StoreError::InstalledDurabilityUnknown => {
                "session state was installed but directory durability is unconfirmed".to_string()
            }
        };
        f.write_str(&crate::redact::scrub_and_bound_diagnostic(&rendered))
    }
}

impl std::fmt::Debug for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for StoreError {}

mod paths;
pub use paths::is_safe_component;

/// The on-disk session store. All state reads and writes happen under one exclusive lock
/// and validate invariants every time.
pub struct SessionStore {
    pub session_dir: PathBuf,
    pub session_id: String,
    dir: openat::Dir,
    file: std::fs::File,
    lock: Mutex<()>,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// A fresh unique owner token: process id + wall-clock nanos is unique per process and
/// never a secret.
fn new_owner() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{now}", std::process::id())
}

fn fchmod(fd: std::os::fd::RawFd, mode: libc::mode_t) -> Result<(), StoreError> {
    if unsafe { libc::fchmod(fd, mode) } != 0 {
        return Err(StoreError::Io(
            "chmod retained session descriptor failed".into(),
        ));
    }
    Ok(())
}

fn open_regular_at(
    dir: &openat::Dir,
    name: &str,
    flags: libc::c_int,
    mode: libc::mode_t,
) -> std::io::Result<std::fs::File> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};

    let name = std::ffi::CString::new(name)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid file name"))?;
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW,
            mode as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    if !file.metadata()?.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "session entry is not a regular file",
        ));
    }
    Ok(file)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StateSlot {
    store_generation: u64,
    state: SessionState,
}

enum SlotRead {
    Missing,
    Valid(StateSlot),
    Corrupt(StoreError),
}

fn read_state_slot(dir: &openat::Dir, name: &str) -> Result<SlotRead, StoreError> {
    let mut bytes = Vec::new();
    let f = match open_regular_at(dir, name, libc::O_RDONLY, 0) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(SlotRead::Missing),
        Err(_) => {
            return Err(StoreError::Io(
                "session state could not be opened safely".into(),
            ));
        }
    };
    f.take(MAX_SESSION_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| StoreError::Io("session state could not be read".into()))?;
    if bytes.len() > MAX_SESSION_BYTES {
        return Ok(SlotRead::Corrupt(StoreError::Corrupt(
            "session state exceeds the session byte cap".into(),
        )));
    }
    let slot = match serde_json::from_slice::<StateSlot>(&bytes) {
        Ok(slot) => slot,
        Err(_) if name == "session.json" => match serde_json::from_slice::<SessionState>(&bytes) {
            Ok(state) => StateSlot {
                store_generation: 0,
                state,
            },
            Err(_) => {
                return Ok(SlotRead::Corrupt(StoreError::Corrupt(
                    "session state is not valid JSON".into(),
                )))
            }
        },
        Err(_) => {
            return Ok(SlotRead::Corrupt(StoreError::Corrupt(
                "session state slot is not valid JSON".into(),
            )))
        }
    };
    if let Err(error) = slot.state.validate() {
        return Ok(SlotRead::Corrupt(error));
    }
    Ok(SlotRead::Valid(slot))
}

fn read_state_with_generation(
    dir: &openat::Dir,
) -> Result<Option<(u64, SessionState)>, StoreError> {
    let primary = read_state_slot(dir, "session.json")?;
    let alternate = read_state_slot(dir, "session.alt.json")?;
    let selected = match (primary, alternate) {
        (SlotRead::Valid(a), SlotRead::Valid(b)) => {
            if a.store_generation >= b.store_generation {
                a
            } else {
                b
            }
        }
        (SlotRead::Valid(slot), _) | (_, SlotRead::Valid(slot)) => slot,
        (SlotRead::Missing, SlotRead::Missing) => return Ok(None),
        (SlotRead::Corrupt(error), _) | (_, SlotRead::Corrupt(error)) => return Err(error),
    };
    Ok(Some((selected.store_generation, selected.state)))
}

fn read_state(dir: &openat::Dir) -> Result<Option<SessionState>, StoreError> {
    Ok(read_state_with_generation(dir)?.map(|(_, state)| state))
}

fn next_store_generation(current: Option<u64>) -> Result<u64, StoreError> {
    match current {
        Some(value) => value
            .checked_add(1)
            .ok_or_else(|| StoreError::Corrupt("session generation overflow".into())),
        None => Ok(0),
    }
}

fn same_file_identity(a: &std::fs::File, b: &std::fs::File) -> Result<bool, StoreError> {
    use std::os::unix::fs::MetadataExt as _;

    let a = a
        .metadata()
        .map_err(|_| StoreError::Io("inspect retained state failed".into()))?;
    let b = b
        .metadata()
        .map_err(|_| StoreError::Io("inspect installed state failed".into()))?;
    Ok((a.dev(), a.ino()) == (b.dev(), b.ino()))
}

fn write_state_slot(dir: &openat::Dir, name: &str, bytes: &[u8]) -> Result<(), StoreError> {
    write_state_slot_inner(
        dir,
        name,
        bytes,
        || {},
        |fd| {
            if unsafe { libc::fsync(fd) } == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        },
    )
}

fn write_state_slot_inner(
    dir: &openat::Dir,
    name: &str,
    bytes: &[u8],
    after_open: impl FnOnce(),
    sync_directory: impl FnOnce(std::os::fd::RawFd) -> std::io::Result<()>,
) -> Result<(), StoreError> {
    use std::io::{Seek as _, Write as _};

    if bytes.len() > MAX_SESSION_BYTES {
        return Err(StoreError::Invalid(format!(
            "session state exceeds the {MAX_SESSION_BYTES} byte cap"
        )));
    }
    use std::os::fd::AsRawFd as _;

    let mut f = open_regular_at(dir, name, libc::O_RDWR | libc::O_CREAT, 0o600)
        .map_err(|_| StoreError::Io("open session state slot failed".into()))?;
    after_open();
    fchmod(f.as_raw_fd(), 0o600)?;
    f.set_len(0)
        .map_err(|_| StoreError::Io("truncate session state slot failed".into()))?;
    f.seek(std::io::SeekFrom::Start(0))
        .map_err(|_| StoreError::Io("seek session state slot failed".into()))?;
    f.write_all(bytes)
        .map_err(|_| StoreError::Io("write session state slot failed".into()))?;
    f.sync_all()
        .map_err(|_| StoreError::Io("sync session state slot failed".into()))?;
    let installed = open_regular_at(dir, name, libc::O_RDONLY, 0)
        .map_err(|_| StoreError::Io("verify session state slot failed".into()))?;
    if !same_file_identity(&f, &installed)? {
        let _ = f.set_len(0);
        let _ = f.sync_all();
        return Err(StoreError::Io(
            "session state slot name was replaced".into(),
        ));
    }
    let syncable_dir = dir
        .open_file(".")
        .map_err(|_| StoreError::InstalledDurabilityUnknown)?;
    sync_directory(syncable_dir.as_raw_fd()).map_err(|_| StoreError::InstalledDurabilityUnknown)?;
    let installed = open_regular_at(dir, name, libc::O_RDONLY, 0)
        .map_err(|_| StoreError::InstalledDurabilityUnknown)?;
    if !same_file_identity(&f, &installed)? {
        let _ = f.set_len(0);
        let _ = f.sync_all();
        return Err(StoreError::InstalledDurabilityUnknown);
    }
    Ok(())
}

fn ensure_private_subdir(parent: &openat::Dir, name: &str) -> Result<openat::Dir, StoreError> {
    if parent.sub_dir(name).is_err() {
        match parent.create_dir(name, 0o700) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => {
                return Err(StoreError::Io("create session directory failed".into()));
            }
        }
    }
    let dir = parent
        .sub_dir(name)
        .map_err(|_| StoreError::Io("open session directory safely failed".into()))?;
    use std::os::fd::AsRawFd as _;
    let permission_handle = dir
        .open_file(".")
        .map_err(|_| StoreError::Io("open retained session descriptor failed".into()))?;
    fchmod(permission_handle.as_raw_fd(), 0o700)?;
    Ok(dir)
}

impl SessionId {
    /// Resolve the session id, rejecting unsafe components.
    pub fn parse(id: &str) -> Result<SessionId, String> {
        if !is_safe_component(id) {
            return Err(format!(
                "invalid session id {id:?}: must be [A-Za-z0-9_-], 1..64 chars"
            ));
        }
        Ok(SessionId { id: id.to_string() })
    }

    /// The on-disk directory for this session.
    pub fn path(&self) -> Result<PathBuf, String> {
        let sessions_dir = paths::sessions_root()?;
        Ok(sessions_dir.join(&self.id))
    }
}

impl SessionStore {
    fn open(session: &SessionId) -> Result<Self, StoreError> {
        let config_path = paths::config_root().map_err(StoreError::Invalid)?;
        std::fs::create_dir_all(&config_path)
            .map_err(|_| StoreError::Io("create configuration directory failed".into()))?;
        let config = crate::tools::open_root(&config_path)
            .map_err(|_| StoreError::Io("open configuration directory safely failed".into()))?;
        let sessions = ensure_private_subdir(&config, "code-rs-sessions")?;
        let dir_cap = ensure_private_subdir(&sessions, &session.id)?;
        let file = open_regular_at(&dir_cap, ".lock", libc::O_RDWR | libc::O_CREAT, 0o600)
            .map_err(|_| StoreError::Lock("lock could not be opened safely".into()))?;
        use std::os::fd::AsRawFd as _;
        fchmod(file.as_raw_fd(), 0o600)?;
        Ok(SessionStore {
            session_dir: config_path.join("code-rs-sessions").join(&session.id),
            session_id: session.id.clone(),
            dir: dir_cap,
            file,
            lock: Mutex::new(()),
        })
    }

    /// Open (or create) the store for a session.
    pub fn load(session: &SessionId) -> Result<SessionStore, StoreError> {
        Self::open(session)
    }

    /// The configuration-root-derived path retained when this store was opened.
    pub fn session_dir(&self) -> &Path {
        &self.session_dir
    }

    /// Require a retained workspace capability to match the identity pinned by this session.
    pub fn verify_workspace_identity(&self, identity: (u64, u64)) -> Result<(), StoreError> {
        self.locked(|| {
            let state = self.read()?;
            if state.cwd.is_none() || (state.cwd_dev, state.cwd_ino) != identity {
                return Err(StoreError::Invalid(
                    "session workspace identity changed".to_string(),
                ));
            }
            Ok(())
        })
    }

    /// Hold the exclusive lock for a bounded critical section. `f` must not call another locked
    /// method.
    fn locked<T>(&self, f: impl FnOnce() -> Result<T, StoreError>) -> Result<T, StoreError> {
        self.locked_with_timeout(SESSION_LOCK_TIMEOUT, f)
    }

    fn locked_with_timeout<T>(
        &self,
        timeout: std::time::Duration,
        f: impl FnOnce() -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let deadline = std::time::Instant::now()
            .checked_add(timeout)
            .ok_or(StoreError::LockTimeout)?;
        let _thread_guard = loop {
            match self.lock.try_lock() {
                Ok(guard) => break guard,
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    return Err(StoreError::Lock("lock poisoned".into()));
                }
                Err(std::sync::TryLockError::WouldBlock) => wait_for_session_lock(deadline)?,
            }
        };
        loop {
            match FileExt::try_lock_exclusive(&self.file) {
                Ok(()) => break,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    wait_for_session_lock(deadline)?;
                }
                Err(_) => return Err(StoreError::Lock("lock operation failed".into())),
            }
        }
        let _file_guard = SessionFileLock(&self.file);
        f()
    }

    /// Read and validate the current state under the exclusive lock. A missing state
    /// file is a fresh empty session; a present-but-corrupt file is an error.
    fn read(&self) -> Result<SessionState, StoreError> {
        Ok(read_state(&self.dir)?.unwrap_or_else(|| SessionState::empty(&self.session_id)))
    }

    fn write(&self, state: &SessionState) -> Result<(), StoreError> {
        state.validate()?;
        let current = read_state_with_generation(&self.dir)?.map(|(generation, _)| generation);
        let generation = next_store_generation(current)?;
        let slot = StateSlot {
            store_generation: generation,
            state: state.clone(),
        };
        let bytes = serde_json::to_vec(&slot)
            .map_err(|e| StoreError::Corrupt(format!("serialize: {e}")))?;
        let name = if generation % 2 == 0 {
            "session.json"
        } else {
            "session.alt.json"
        };
        write_state_slot(&self.dir, name, &bytes)
    }

    /// The chain of `start`'s ancestors from the root down to `start` (inclusive),
    /// by following `parent_branch` links.
    fn lineage(state: &SessionState, start: usize) -> Vec<usize> {
        let mut chain = vec![start];
        let mut cur = start;
        let mut guard = 0usize;
        while guard < state.branches.len() {
            guard += 1;
            let next = match &state.branches[cur].parent_branch {
                Some(p) => match state.branches.iter().position(|b| &b.branch_id == p) {
                    Some(i) => i,
                    None => break,
                },
                None => break,
            };
            chain.push(next);
            cur = next;
        }
        chain.reverse();
        chain
    }

    /// Collapse a lineage chain (root..leaf) to the **last** branch per turn, so a
    /// re-run of an earlier turn becomes the sole representative of its turn and older
    /// sibling attempts are excluded.
    fn collapse_per_turn(branches: &[BranchRecord], chain: &[usize]) -> Vec<usize> {
        let mut by_turn: Vec<(u32, usize)> = chain.iter().map(|&i| (branches[i].turn, i)).collect();
        by_turn.sort_by_key(|(t, i)| (*t, *i));
        // Newest first, then dedup keeps the newest per turn.
        by_turn.reverse();
        by_turn.dedup_by_key(|(t, _)| *t);
        by_turn.reverse();
        let mut out: Vec<usize> = by_turn.into_iter().map(|(_, i)| i).collect();
        out.sort_by_key(|&i| branches[i].turn);
        out
    }

    /// Resolve the selected branch (must be completed), else the newest completed branch.
    fn select_current(
        state: &SessionState,
        branch: Option<&str>,
    ) -> Result<Option<usize>, StoreError> {
        match branch {
            Some(b) => {
                let i = state
                    .branches
                    .iter()
                    .position(|x| x.branch_id == b)
                    .ok_or_else(|| StoreError::Invalid(format!("unknown branch {b}")))?;
                if state.branches[i].lifecycle != Lifecycle::Completed {
                    return Err(StoreError::Invalid(format!(
                        "selected branch {b} is not completed"
                    )));
                }
                Ok(Some(i))
            }
            None => Ok(state
                .branches
                .iter()
                .rposition(|b| b.lifecycle == Lifecycle::Completed)),
        }
    }

    /// The latest turn in the collapsed selected lineage (the turn of `current`).
    fn lineage_latest(branches: &[BranchRecord], current: usize) -> u32 {
        branches[current].turn
    }

    /// Resolve the predecessor branch for a fork/retry at `target`: the branch in the
    /// selected lineage at turn `target - 1` (the immediate lower turn), so an explicit
    /// re-run of an earlier turn parents to its own predecessor, never a later turn.
    fn predecessor_at(branches: &[BranchRecord], chain: &[usize], target: u32) -> Option<usize> {
        let pred_turn = target.checked_sub(1)?;
        chain
            .iter()
            .copied()
            .find(|&j| branches[j].turn == pred_turn)
    }

    /// Resolve the existing branch at `target` with the identical prompt that continues
    /// the **selected lineage**: either an ancestor of `current` at `target` (a replay
    /// or fork at or before it), or the newest child of the lineage's branch at
    /// `target - 1` (the predecessor). The child case is what makes a `pending`
    /// branch at a later turn visible to a second store, so a live lease returns
    /// `Busy` (never a duplicate reservation) and a stale lease is reclaimed in
    /// place; a failed child retries. A sibling lineage with the same prompt is never a
    /// match.
    fn find_existing(
        state: &SessionState,
        current: Option<usize>,
        target: u32,
        prompt: &str,
    ) -> Option<usize> {
        let same = |b: &BranchRecord| b.turn == target && b.prompt == prompt;
        match current {
            None => state.branches.iter().rposition(same),
            Some(i) => {
                let chain = Self::lineage(state, i);
                let collapsed = Self::collapse_per_turn(&state.branches, &chain);
                // An ancestor at `target` (the current turn or an earlier one).
                let ancestor = collapsed.iter().rev().find(|&&j| {
                    state.branches[j].turn == target && state.branches[j].prompt == prompt
                });
                if let Some(&j) = ancestor {
                    return Some(j);
                }
                // The continuation at a later turn: the newest child of the
                // predecessor branch at `target - 1` with the same prompt.
                let pred_turn = target.checked_sub(1)?;
                let pred = collapsed
                    .iter()
                    .copied()
                    .find(|&j| state.branches[j].turn == pred_turn)?;
                let pred_id = state.branches[pred].branch_id.as_str();
                state.branches.iter().rposition(|b| {
                    b.turn == target
                        && b.parent_branch.as_deref() == Some(pred_id)
                        && b.prompt == prompt
                })
            }
        }
    }

    /// Resolve completed prior-turn history for a new branch at `target`: the collapsed
    /// lineage of `current`, restricted to turns strictly below `target`.
    fn prior_history(
        &self,
        state: &SessionState,
        current: Option<usize>,
        target: u32,
    ) -> Vec<HistoryTurn> {
        match current {
            None => Vec::new(),
            Some(i) => {
                let chain = Self::lineage(state, i);
                Self::collapse_per_turn(&state.branches, &chain)
                    .into_iter()
                    .filter(|&j| state.branches[j].turn < target)
                    .map(|j| {
                        let b = &state.branches[j];
                        HistoryTurn {
                            turn: b.turn,
                            attempt: b.attempt,
                            branch_id: b.branch_id.clone(),
                            prompt: b.prompt.clone(),
                            rounds: b.rounds.clone(),
                            summary: b.summary.clone(),
                        }
                    })
                    .collect()
            }
        }
    }

    /// Renew the lease of a live reservation under its owner token. `Stale` is returned
    /// when the branch is gone, no longer pending, or owned by a different token. The caller
    /// renews before/after every model request and at every checkpoint.
    pub fn renew_lease(&self, reserved: &ReservedRequest) -> Result<(), StoreError> {
        self.locked(|| {
            let mut state = self.read()?;
            let idx = state
                .branches
                .iter()
                .position(|b| b.branch_id == reserved.branch_id)
                .ok_or(StoreError::Stale)?;
            let t = &mut state.branches[idx];
            if t.lifecycle != Lifecycle::Pending || t.owner != reserved.owner {
                return Err(StoreError::Stale);
            }
            if t.turn != reserved.turn || t.digest != crate::agent::prompt_digest(&reserved.prompt)
            {
                return Err(StoreError::Stale);
            }
            let now = now_secs();
            // A lease that already expired is stale even if the owner token still matches: the
            // branch may have been reclaimed. Renew it only while it is still ours and live.
            if t.lease_expiry <= now {
                return Err(StoreError::Stale);
            }
            t.lease_expiry = now.saturating_add(LEASE_SECONDS);
            self.write(&state)
        })
    }

    /// Persist the current partial rounds without completing the attempt, so a crash
    /// between model rounds cannot lose the transcript. The attempt stays `pending`. The
    /// caller's owner token must still match.
    pub fn checkpoint(
        &self,
        reserved: &ReservedRequest,
        rounds: &[RoundRecord],
    ) -> Result<(), StoreError> {
        self.locked(|| {
            let mut state = self.read()?;
            let idx = state
                .branches
                .iter()
                .position(|b| b.branch_id == reserved.branch_id)
                .ok_or(StoreError::Stale)?;
            let t = &mut state.branches[idx];
            if t.lifecycle != Lifecycle::Pending || t.owner != reserved.owner {
                return Err(StoreError::Stale);
            }
            if t.turn != reserved.turn || t.digest != crate::agent::prompt_digest(&reserved.prompt)
            {
                return Err(StoreError::Stale);
            }
            let now = now_secs();
            // A lease that already expired is stale even if the owner token still matches:
            // the branch may have been reclaimed. Renew it only while it is still ours and
            // live, so a checkpoint both persists the partial transcript and extends the
            // lease under the same lock.
            if t.lease_expiry <= now {
                return Err(StoreError::Stale);
            }
            t.rounds = rounds.to_vec();
            t.lease_expiry = now.saturating_add(LEASE_SECONDS);
            self.write(&state)
        })
    }

    /// Persist a completed branch (full assistant/tool history + summary) under the lock,
    /// verifying the owner token.
    pub fn finalize(
        &self,
        reserved: &ReservedRequest,
        summary: &str,
        rounds: &[RoundRecord],
    ) -> Result<(), StoreError> {
        self.locked(|| {
            let mut state = self.read()?;
            let idx = state
                .branches
                .iter()
                .position(|b| b.branch_id == reserved.branch_id)
                .ok_or(StoreError::Stale)?;
            let t = &mut state.branches[idx];
            if t.lifecycle != Lifecycle::Pending || t.owner != reserved.owner {
                return Err(StoreError::Stale);
            }
            if t.turn != reserved.turn || t.digest != crate::agent::prompt_digest(&reserved.prompt)
            {
                return Err(StoreError::Stale);
            }
            let now = now_secs();
            // A lease that is already at or past now is stale: the reservation may
            // have been reclaimed, so the commit is rejected before any field is
            // mutated. Renewal/checkpoint gate the same way, keeping an expired
            // owner unable to finalize a branch another process took over.
            if t.lease_expiry <= now {
                return Err(StoreError::Stale);
            }
            t.rounds = rounds.to_vec();
            t.summary = summary.to_string();
            t.lifecycle = Lifecycle::Completed;
            t.owner.clear();
            self.write(&state)
        })
    }

    /// Persist a terminal failure on this reservation, verifying the owner token.
    pub fn fail(
        &self,
        reserved: &ReservedRequest,
        error: &str,
        rounds: &[RoundRecord],
    ) -> Result<(), StoreError> {
        self.locked(|| {
            let mut state = self.read()?;
            let idx = state
                .branches
                .iter()
                .position(|b| b.branch_id == reserved.branch_id)
                .ok_or(StoreError::Stale)?;
            let t = &mut state.branches[idx];
            if t.lifecycle != Lifecycle::Pending || t.owner != reserved.owner {
                return Err(StoreError::Stale);
            }
            if t.turn != reserved.turn || t.digest != crate::agent::prompt_digest(&reserved.prompt)
            {
                return Err(StoreError::Stale);
            }
            let now = now_secs();
            // A lease that is already at or past now is stale: the reservation may
            // have been reclaimed, so the failure is rejected before any field is
            // mutated. Renewal/checkpoint gate the same way, keeping an expired
            // owner unable to fail a branch another process took over.
            if t.lease_expiry <= now {
                return Err(StoreError::Stale);
            }
            t.rounds = rounds.to_vec();
            t.error = error.to_string();
            t.lifecycle = Lifecycle::Failed;
            t.owner.clear();
            self.write(&state)
        })
    }

    /// Snapshot the current validated state (used by the CLI for cwd checks).
    pub fn snapshot(&self) -> Result<SessionState, StoreError> {
        self.locked(|| self.read())
    }
}

fn wait_for_session_lock(deadline: std::time::Instant) -> Result<(), StoreError> {
    let now = std::time::Instant::now();
    if now >= deadline {
        return Err(StoreError::LockTimeout);
    }
    std::thread::sleep(SESSION_LOCK_RETRY.min(deadline.duration_since(now)));
    Ok(())
}

struct SessionFileLock<'a>(&'a std::fs::File);

impl Drop for SessionFileLock<'_> {
    fn drop(&mut self) {
        let _ = FileExt::unlock(self.0);
    }
}

/// Convenience: open a store for a session (used by the CLI).
pub fn load_session_store(session: &SessionId) -> Result<SessionStore, String> {
    SessionStore::load(session).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests;
