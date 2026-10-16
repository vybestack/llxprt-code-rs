//! Read-only manifest selection. Rendering happens after the shared lock is released.
use super::*;

pub(in crate::session) fn read_transcript(
    session: &SessionId,
    root: &Path,
) -> Result<SessionState, StoreError> {
    let config = crate::tools::open_root(root).map_err(StoreError::Io)?;
    let sessions = config
        .sub_dir("code-rs-sessions")
        .map_err(|_| StoreError::Io("open existing sessions directory failed".into()))?;
    let dir = sessions
        .sub_dir(&session.id)
        .map_err(|_| StoreError::Io("open existing session directory failed".into()))?;
    let file = open_regular_at(&dir, ".lock", libc::O_RDONLY, 0)
        .map_err(|_| StoreError::Io("open existing session lock failed".into()))?;
    let deadline = Instant::now()
        .checked_add(SESSION_LOCK_TIMEOUT)
        .ok_or(StoreError::LockTimeout)?;
    loop {
        match FileExt::try_lock_shared(&file) {
            Ok(()) => break,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == ErrorKind::WouldBlock => wait_for_lock(deadline)?,
            Err(_) => return Err(StoreError::Lock("read lock failed".into())),
        }
    }
    let _guard = SessionFileLock(&file);
    let manifest = read_manifest(&dir)?
        .ok_or_else(|| StoreError::Corrupt("session manifest is missing".into()))?;
    let loaded = match load_set_mode(&dir, &manifest, &manifest.current, true, false) {
        Ok(current) => current,
        Err(current_error) => {
            let previous = manifest
                .previous
                .as_ref()
                .ok_or_else(|| StoreError::Corrupt(current_error.to_string()))?;
            load_set_mode(&dir, &manifest, previous, false, false)
                .map_err(|previous_error| combined_recovery_error(current_error, previous_error))?
        }
    };
    if loaded.state.session_id != session.id {
        return Err(StoreError::Corrupt("session id mismatch".into()));
    }
    Ok(loaded.state)
}

/// Identify an already-published current-format snapshot for an error envelope.
/// No creation, repair, lock acquisition or transcript replay: even a busy session or
/// corrupt log can still declare its format. Unidentifiable state is not mislabeled.
pub(crate) fn persisted_version(session_id: &str) -> Option<u32> {
    let session = SessionId::parse(session_id).ok()?;
    let root = crate::config::std_profile_dir().ok()?;
    let config = crate::tools::open_root(&root).ok()?;
    let sessions = config.sub_dir("code-rs-sessions").ok()?;
    let dir = sessions.sub_dir(&session.id).ok()?;
    let manifest = read_manifest(&dir).ok()??;
    let bytes = read_artifact(&dir, &manifest.current.snapshot, MAX_SESSION_BYTES + 4096).ok()?;
    if log::full_digest(&bytes) != manifest.current.snapshot_digest {
        return None;
    }
    let snapshot: SnapshotFile = serde_json::from_slice(&bytes).ok()?;
    (snapshot.format_version == log::FORMAT_VERSION
        && snapshot.state.session_id == session.id
        && snapshot.state.version == STORE_VERSION)
        .then_some(snapshot.state.version)
}
