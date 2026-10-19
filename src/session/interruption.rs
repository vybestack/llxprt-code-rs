//! Typed, owner-scoped interruption evidence, separate from the transactional history.
//! Signal handlers cannot safely acquire the session lock or serialize a turn.
use super::{BranchRecord, ReservedRequest, SessionStore, StoreError};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

mod projection;

pub(crate) const MAX_ARGS_BYTES: usize = 8192;
const MAX_RECORD_BYTES: usize = MAX_ARGS_BYTES * 8 + 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct InFlight {
    pub tool: String,
    pub id: String,
    pub turn: u32,
    pub round: usize,
    pub started: u64,
    /// Transcript-style redacted, bounded argument JSON text (possibly truncated).
    pub args: String,
}

#[derive(Serialize)]
pub(crate) struct Cancellation {
    pub signal: &'static str,
    pub in_flight: Option<InFlight>,
}

fn name(owner: &str, suffix: &str) -> String {
    use sha2::{Digest, Sha256};
    format!(
        "interruption-{:x}.{suffix}",
        Sha256::digest(owner.as_bytes())
    )
}

pub(crate) fn tool_path(dir: &Path, owner: &str) -> PathBuf {
    dir.join(name(owner, "tool.json"))
}

/// Fail before starting the turn if its signal evidence cannot be armed.
/// Replay does not execute a turn and must not replace any earlier evidence.
pub(crate) fn arm(
    store: &SessionStore,
    reserved: &ReservedRequest,
) -> Result<Option<crate::process::CancellationRecord>, StoreError> {
    if reserved.replay {
        return Ok(None);
    }
    crate::process::CancellationRecord::install(prepare(store, reserved)?)
        .map(Some)
        .map_err(StoreError::Io)
}

fn prepare(store: &SessionStore, reserved: &ReservedRequest) -> Result<std::fs::File, StoreError> {
    owned(store, reserved, || {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(store.session_dir().join(name(&reserved.owner, "signal")))?;
        file.try_lock_exclusive()?;
        file.sync_all()?;
        projection::retire_inactive(store.session_dir())?;
        Ok(file)
    })
}

pub(crate) fn publish(dir: &Path, filename: &str, bytes: &[u8]) -> std::io::Result<()> {
    let dir = openat::Dir::open(dir)?;
    crate::safe_file::publish_artifact(&dir, filename, bytes, |dir, name, flags, mode| {
        super::open_regular_at(dir, name, flags, mode)
    })
}

pub(crate) fn mark(
    store: &SessionStore,
    reserved: &ReservedRequest,
    flight: &InFlight,
) -> Result<(), StoreError> {
    owned(store, reserved, || {
        let bytes = serde_json::to_vec(flight)?;
        publish(
            store.session_dir(),
            &name(&reserved.owner, "tool.json"),
            &bytes,
        )?;
        projection::mark(store.session_dir(), &reserved.owner, flight)
    })
}

pub(crate) fn clear(store: &SessionStore, reserved: &ReservedRequest) -> Result<(), StoreError> {
    owned(store, reserved, || {
        // A signal between calls must not attribute cancellation to a completed tool.
        publish(
            store.session_dir(),
            &name(&reserved.owner, "tool.json"),
            b"null",
        )?;
        projection::clear_owned(store.session_dir(), &reserved.owner)
    })
}

fn owned<T>(
    store: &SessionStore,
    reserved: &ReservedRequest,
    operation: impl FnOnce() -> std::io::Result<T>,
) -> Result<T, StoreError> {
    store.locked(|| {
        let state = store.read()?;
        store.live_branch(&state, reserved)?;
        operation().map_err(|error| StoreError::Io(error.to_string()))
    })
}

/// Read without repairing the session. Owner hashing prevents reclaimed reservations
/// or later branches from borrowing another worker's marker.
pub(crate) fn read(dir: &Path, branch: &BranchRecord) -> std::io::Result<Option<Cancellation>> {
    if branch.lifecycle != super::Lifecycle::Pending {
        return Ok(None);
    }
    let file =
        match crate::safe_file::open_regular_nofollow(&dir.join(name(&branch.owner, "signal"))) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
    let mut bytes = Vec::new();
    file.take(2).read_to_end(&mut bytes)?;
    let signal = match bytes.as_slice() {
        [] => return Ok(None),
        b"I" => "SIGINT",
        b"T" => "SIGTERM",
        _ => return Err(std::io::Error::other("invalid cancellation signal record")),
    };
    let in_flight = match crate::safe_file::open_regular_nofollow(&tool_path(dir, &branch.owner)) {
        Ok(file) => {
            let mut bytes = Vec::new();
            file.take((MAX_RECORD_BYTES + 1) as u64)
                .read_to_end(&mut bytes)?;
            if bytes.len() > MAX_RECORD_BYTES {
                return Err(std::io::Error::other(
                    "in-flight record exceeds its byte cap",
                ));
            }
            serde_json::from_slice::<Option<InFlight>>(&bytes)?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    Ok(Some(Cancellation { signal, in_flight }))
}

#[cfg(test)]
mod tests;
