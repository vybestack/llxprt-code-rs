//! Snapshots, manifest publication, migration, recovery-set selection, and compaction.

use super::*;
use log::{ReplayCursor, ReplayResult};
use serde::{Deserialize, Serialize};
use std::io::Write as _;
use std::os::fd::AsRawFd as _;

const MANIFEST: &str = "session.manifest.json";
const MANIFEST_TEMP: &str = ".session.manifest.tmp";

#[derive(Clone, Serialize, Deserialize)]
struct SnapshotFile {
    format_version: u32,
    base_seq: u64,
    last_seq: u64,
    last_frame_digest: [u8; 16],
    state: SessionState,
}

#[derive(Clone, Serialize, Deserialize)]
struct RecoverySet {
    snapshot: String,
    snapshot_digest: String,
    base_seq: u64,
    last_seq: u64,
    segment: String,
    first_seq: u64,
    #[serde(default)]
    sealed_len: Option<u64>,
    #[serde(default)]
    sealed_digest: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Manifest {
    format_version: u32,
    generation: u64,
    current: RecoverySet,
    previous: Option<RecoverySet>,
}

pub(super) struct LoadedStore {
    manifest: Manifest,
    pub state: SessionState,
    pub cursor: ReplayCursor,
    segment_identity: (u64, u64),
    pub repaired_tail: bool,
}

pub(super) fn load_or_migrate(
    dir: &openat::Dir,
    session_id: &str,
) -> Result<LoadedStore, StoreError> {
    match read_manifest(dir)? {
        Some(manifest) => {
            let loaded = match load_set(dir, &manifest, &manifest.current, true) {
                Ok(loaded) => loaded,
                Err(current_error) => {
                    let Some(previous) = manifest.previous.as_ref() else {
                        return Err(current_error);
                    };
                    match load_set(dir, &manifest, previous, false) {
                        Ok(previous_loaded) => {
                            recover_previous(dir, previous_loaded, previous.clone())?
                        }
                        Err(previous_error) => {
                            return Err(combined_recovery_error(current_error, previous_error));
                        }
                    }
                }
            };
            cleanup_legacy(dir)?;
            Ok(loaded)
        }
        None => migrate_legacy(dir, session_id),
    }
}

pub(super) fn replace_materialized(
    dir: &openat::Dir,
    state: &SessionState,
) -> Result<LoadedStore, StoreError> {
    let loaded = load_or_migrate(dir, &state.session_id)?;
    let previous = seal_current(dir, &loaded)?;
    let manifest = initial_manifest(
        dir,
        state,
        loaded.cursor.seq,
        loaded.cursor.digest,
        Some(previous),
    )?;
    load_set(dir, &manifest, &manifest.current, true)
}

pub(super) fn catch_up(dir: &openat::Dir, loaded: &mut LoadedStore) -> Result<(), StoreError> {
    let manifest = read_manifest(dir)?.ok_or_else(|| {
        StoreError::Corrupt("session manifest disappeared after migration".into())
    })?;
    if manifest.generation != loaded.manifest.generation
        || manifest.current.segment != loaded.manifest.current.segment
    {
        *loaded = load_or_migrate(dir, &loaded.state.session_id)?;
        return Ok(());
    }
    let mut file = super::open_regular_at(dir, &manifest.current.segment, libc::O_RDWR, 0)
        .map_err(|_| StoreError::Io("open active session segment failed".into()))?;
    if log::identity(&file)? != loaded.segment_identity {
        *loaded = load_or_migrate(dir, &loaded.state.session_id)?;
        return Ok(());
    }
    let mut candidate = loaded.state.clone();
    let result = log::replay_from(
        &mut file,
        &mut candidate,
        ReplayCursor {
            seq: loaded.cursor.seq,
            offset: loaded.cursor.offset,
            digest: loaded.cursor.digest,
            events: loaded.cursor.events,
        },
        true,
    );
    match result {
        Ok(result) => {
            loaded.state = candidate;
            loaded.cursor = result.cursor;
            loaded.repaired_tail |= result.repaired_tail;
            Ok(())
        }
        Err(error @ StoreError::Corrupt(_)) => {
            let Some(previous) = manifest.previous.as_ref() else {
                return Err(error);
            };
            match load_set(dir, &manifest, previous, false) {
                Ok(previous_loaded) => {
                    *loaded = recover_previous(dir, previous_loaded, previous.clone())?;
                    Ok(())
                }
                Err(previous_error) => Err(combined_recovery_error(error, previous_error)),
            }
        }
        Err(error) => Err(error),
    }
}

pub(super) fn append(
    dir: &openat::Dir,
    loaded: &mut LoadedStore,
    events: Vec<log::Event>,
) -> Result<(), StoreError> {
    catch_up(dir, loaded)?;
    let seq = loaded
        .cursor
        .seq
        .checked_add(1)
        .ok_or_else(|| StoreError::Corrupt("session sequence overflow".into()))?;
    let frame = log::encode_frame(seq, loaded.cursor.digest, &events)?;
    let mut candidate = loaded.state.clone();
    replay::apply_batch(&mut candidate, &frame.batch)?;
    check_logical_write(&candidate)?;
    let offset = loaded
        .cursor
        .offset
        .checked_add(frame.bytes.len() as u64)
        .ok_or_else(|| StoreError::Corrupt("session segment offset overflow".into()))?;
    let event_count = loaded
        .cursor
        .events
        .checked_add(1)
        .ok_or_else(|| StoreError::Corrupt("session event count overflow".into()))?;
    log::append_frame(
        dir,
        &loaded.manifest.current.segment,
        loaded.cursor.offset,
        &frame,
    )?;
    loaded.state = candidate;
    loaded.cursor.seq = seq;
    loaded.cursor.offset = offset;
    loaded.cursor.digest = frame.digest;
    loaded.cursor.events = event_count;
    if loaded.cursor.offset >= log::SEGMENT_BYTE_THRESHOLD
        || loaded.cursor.events >= log::EVENT_THRESHOLD
    {
        compact(dir, loaded).map_err(|error| {
            StoreError::CommittedMaintenance(format!("snapshot rotation failed: {error}"))
        })?;
    }
    Ok(())
}

fn migrate_legacy(dir: &openat::Dir, session_id: &str) -> Result<LoadedStore, StoreError> {
    let state = super::read_legacy_state(dir)?.unwrap_or_else(|| SessionState::empty(session_id));
    check_logical_read(&state)?;
    let manifest = initial_manifest(dir, &state, 0, [0; 16], None)?;
    let loaded = load_set(dir, &manifest, &manifest.current, true)?;
    cleanup_legacy(dir)?;
    Ok(loaded)
}

fn recover_previous(
    dir: &openat::Dir,
    mut loaded: LoadedStore,
    previous: RecoverySet,
) -> Result<LoadedStore, StoreError> {
    let manifest = initial_manifest(
        dir,
        &loaded.state,
        loaded.cursor.seq,
        loaded.cursor.digest,
        Some(previous),
    )?;
    loaded = load_set(dir, &manifest, &manifest.current, true)?;
    cleanup_legacy(dir)?;
    Ok(loaded)
}

fn combined_recovery_error(current: StoreError, previous: StoreError) -> StoreError {
    let io = matches!(&current, StoreError::Io(_)) || matches!(&previous, StoreError::Io(_));
    let message = format!(
        "current recovery set failed ({current}); retained recovery set failed ({previous})"
    );
    if io {
        StoreError::Io(message)
    } else {
        StoreError::Corrupt(message)
    }
}

fn seal_current(dir: &openat::Dir, loaded: &LoadedStore) -> Result<RecoverySet, StoreError> {
    let mut sealed = loaded.manifest.current.clone();
    let bytes = read_artifact(
        dir,
        &sealed.segment,
        log::SEGMENT_BYTE_THRESHOLD as usize + MAX_SESSION_BYTES + 4096,
    )?;
    if bytes.len() as u64 != loaded.cursor.offset {
        return Err(StoreError::Corrupt(
            "active session segment length is inconsistent".into(),
        ));
    }
    sealed.last_seq = loaded.cursor.seq;
    sealed.sealed_len = Some(bytes.len() as u64);
    sealed.sealed_digest = Some(log::full_digest(&bytes));
    Ok(sealed)
}

#[cfg(test)]
pub(super) fn test_initial_manifest(
    dir: &openat::Dir,
    state: &SessionState,
) -> Result<(), StoreError> {
    initial_manifest(dir, state, 0, [0; 16], None).map(|_| ())
}

fn initial_manifest(
    dir: &openat::Dir,
    state: &SessionState,
    seq: u64,
    frame_digest: [u8; 16],
    previous: Option<RecoverySet>,
) -> Result<Manifest, StoreError> {
    state.validate()?;
    check_logical_write(state)?;
    let generation = match read_manifest(dir)? {
        Some(value) => value
            .generation
            .checked_add(1)
            .ok_or_else(|| StoreError::Corrupt("manifest generation overflow".into()))?,
        None => 0,
    };
    let snapshot = format!("snapshot-{generation}-{seq}.json");
    let segment = format!("segment-{generation}-{}.log", seq.saturating_add(1));
    let snapshot_digest = write_snapshot(dir, &snapshot, state, seq, frame_digest)?;
    create_empty(dir, &segment)?;
    let current = RecoverySet {
        snapshot,
        snapshot_digest,
        base_seq: seq,
        last_seq: seq,
        segment,
        first_seq: seq.saturating_add(1),
        sealed_len: None,
        sealed_digest: None,
    };
    let manifest = Manifest {
        format_version: log::FORMAT_VERSION,
        generation,
        current,
        previous,
    };
    publish_manifest(dir, &manifest)?;
    Ok(manifest)
}

fn compact(dir: &openat::Dir, loaded: &mut LoadedStore) -> Result<(), StoreError> {
    loaded.state.validate()?;
    check_logical_write(&loaded.state)?;
    let old_previous = loaded.manifest.previous.clone();
    let previous = seal_current(dir, loaded)?;
    let generation = loaded
        .manifest
        .generation
        .checked_add(1)
        .ok_or_else(|| StoreError::Corrupt("manifest generation overflow".into()))?;
    let snapshot = format!("snapshot-{generation}-{}.json", loaded.cursor.seq);
    let segment = format!(
        "segment-{generation}-{}.log",
        loaded.cursor.seq.saturating_add(1)
    );
    let snapshot_digest = write_snapshot(
        dir,
        &snapshot,
        &loaded.state,
        loaded.cursor.seq,
        loaded.cursor.digest,
    )?;
    create_empty(dir, &segment)?;
    let manifest = Manifest {
        format_version: log::FORMAT_VERSION,
        generation,
        current: RecoverySet {
            snapshot,
            snapshot_digest,
            base_seq: loaded.cursor.seq,
            last_seq: loaded.cursor.seq,
            segment,
            first_seq: loaded.cursor.seq.saturating_add(1),
            sealed_len: None,
            sealed_digest: None,
        },
        previous: Some(previous),
    };
    publish_manifest(dir, &manifest)?;
    if let Some(old) = old_previous {
        remove_set(dir, &old, &manifest)?;
    }
    *loaded = load_set(dir, &manifest, &manifest.current, true)?;
    Ok(())
}

fn load_set(
    dir: &openat::Dir,
    manifest: &Manifest,
    set: &RecoverySet,
    active: bool,
) -> Result<LoadedStore, StoreError> {
    if manifest.format_version != log::FORMAT_VERSION || set.base_seq != set.last_seq && active {
        return Err(StoreError::Corrupt(
            "unsupported or inconsistent session manifest".into(),
        ));
    }
    let snapshot_bytes = read_artifact(dir, &set.snapshot, MAX_SESSION_BYTES + 4096)?;
    if log::full_digest(&snapshot_bytes) != set.snapshot_digest {
        return Err(StoreError::Corrupt(
            "session snapshot digest mismatch".into(),
        ));
    }
    let snapshot: SnapshotFile = serde_json::from_slice(&snapshot_bytes)
        .map_err(|_| StoreError::Corrupt("session snapshot is invalid".into()))?;
    if snapshot.format_version != log::FORMAT_VERSION
        || snapshot.base_seq != set.base_seq
        || snapshot.last_seq != set.base_seq
    {
        return Err(StoreError::Corrupt(
            "session snapshot metadata is inconsistent".into(),
        ));
    }
    check_logical_read(&snapshot.state)?;
    let mut segment = open_segment(dir, set, active)?;
    if let (Some(length), Some(digest)) = (set.sealed_len, set.sealed_digest.as_ref()) {
        let bytes = read_artifact(
            dir,
            &set.segment,
            log::SEGMENT_BYTE_THRESHOLD as usize + MAX_SESSION_BYTES + 4096,
        )?;
        if bytes.len() as u64 != length || log::full_digest(&bytes) != *digest {
            return Err(StoreError::Corrupt(
                "sealed session segment digest mismatch".into(),
            ));
        }
    } else if !active {
        return Err(StoreError::Corrupt(
            "retained session segment is not sealed".into(),
        ));
    }
    let identity = log::identity(&segment)?;
    let mut state = snapshot.state;
    let ReplayResult {
        cursor,
        repaired_tail,
    } = log::replay_from(
        &mut segment,
        &mut state,
        ReplayCursor {
            seq: snapshot.last_seq,
            offset: 0,
            digest: snapshot.last_frame_digest,
            events: 0,
        },
        active,
    )?;
    if !active && cursor.seq != set.last_seq {
        return Err(StoreError::Corrupt(
            "retained session segment sequence mismatch".into(),
        ));
    }
    Ok(LoadedStore {
        manifest: manifest.clone(),
        state,
        cursor,
        segment_identity: identity,
        repaired_tail,
    })
}

fn open_segment(
    dir: &openat::Dir,
    set: &RecoverySet,
    active: bool,
) -> Result<std::fs::File, StoreError> {
    let flags = if active { libc::O_RDWR } else { libc::O_RDONLY };
    super::open_regular_at(dir, &set.segment, flags, 0).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            StoreError::Corrupt("session segment is missing".into())
        } else {
            StoreError::Io("open session segment safely failed".into())
        }
    })
}

fn write_snapshot(
    dir: &openat::Dir,
    name: &str,
    state: &SessionState,
    seq: u64,
    digest: [u8; 16],
) -> Result<String, StoreError> {
    state.validate()?;
    check_logical_write(state)?;
    let value = SnapshotFile {
        format_version: log::FORMAT_VERSION,
        base_seq: seq,
        last_seq: seq,
        last_frame_digest: digest,
        state: state.clone(),
    };
    let bytes = serde_json::to_vec(&value)
        .map_err(|error| StoreError::Corrupt(format!("serialize snapshot: {error}")))?;
    let temp = format!(".{name}.tmp");
    write_temp_rename(dir, &temp, name, &bytes)?;
    Ok(log::full_digest(&bytes))
}

fn publish_manifest(dir: &openat::Dir, manifest: &Manifest) -> Result<(), StoreError> {
    let bytes = serde_json::to_vec(manifest)
        .map_err(|error| StoreError::Corrupt(format!("serialize manifest: {error}")))?;
    write_temp_rename(dir, MANIFEST_TEMP, MANIFEST, &bytes)
}

fn write_temp_rename(
    dir: &openat::Dir,
    temp: &str,
    name: &str,
    bytes: &[u8],
) -> Result<(), StoreError> {
    let _ = dir.remove_file(temp);
    let mut file = super::open_regular_at(
        dir,
        temp,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        0o600,
    )
    .map_err(|_| StoreError::Io("create session artifact temporary failed".into()))?;
    super::fchmod(file.as_raw_fd(), 0o600)?;
    file.write_all(bytes)
        .map_err(|_| StoreError::Io("write session artifact failed".into()))?;
    file.sync_all()
        .map_err(|_| StoreError::Io("sync session artifact failed".into()))?;
    dir.local_rename(temp, name)
        .map_err(|_| StoreError::Io("publish session artifact failed".into()))?;
    sync_dir(dir)?;
    let installed = super::open_regular_at(dir, name, libc::O_RDONLY, 0)
        .map_err(|_| StoreError::InstalledDurabilityUnknown)?;
    if !super::same_file_identity(&file, &installed)? {
        return Err(StoreError::InstalledDurabilityUnknown);
    }
    Ok(())
}

fn create_empty(dir: &openat::Dir, name: &str) -> Result<(), StoreError> {
    let file = super::open_regular_at(
        dir,
        name,
        libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
        0o600,
    )
    .map_err(|_| StoreError::Io("create session segment failed".into()))?;
    super::fchmod(file.as_raw_fd(), 0o600)?;
    file.sync_all()
        .map_err(|_| StoreError::Io("sync new session segment failed".into()))?;
    sync_dir(dir)
}

fn read_manifest(dir: &openat::Dir) -> Result<Option<Manifest>, StoreError> {
    let bytes = match read_optional(dir, MANIFEST, 256 * 1024)? {
        Some(bytes) => bytes,
        None => return Ok(None),
    };
    let manifest: Manifest = serde_json::from_slice(&bytes)
        .map_err(|_| StoreError::Corrupt("session manifest is invalid".into()))?;
    if manifest.format_version != log::FORMAT_VERSION {
        return Err(StoreError::Corrupt(
            "unsupported session manifest version".into(),
        ));
    }
    Ok(Some(manifest))
}

fn read_artifact(dir: &openat::Dir, name: &str, cap: usize) -> Result<Vec<u8>, StoreError> {
    read_optional(dir, name, cap)?
        .ok_or_else(|| StoreError::Corrupt("session artifact is missing".into()))
}

fn read_optional(dir: &openat::Dir, name: &str, cap: usize) -> Result<Option<Vec<u8>>, StoreError> {
    let file = match super::open_regular_at(dir, name, libc::O_RDONLY, 0) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(StoreError::Io("open session artifact safely failed".into())),
    };
    let mut bytes = Vec::new();
    file.take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| StoreError::Io("read session artifact failed".into()))?;
    if bytes.len() > cap {
        return Err(StoreError::Corrupt(
            "session artifact exceeds its byte cap".into(),
        ));
    }
    Ok(Some(bytes))
}

fn check_logical_read(state: &SessionState) -> Result<(), StoreError> {
    state.validate()?;
    let bytes = serde_json::to_vec(state)
        .map_err(|_| StoreError::Corrupt("serialize materialized session state failed".into()))?;
    if bytes.len() > MAX_SESSION_BYTES {
        return Err(StoreError::Corrupt(
            "session state exceeds the session byte cap".into(),
        ));
    }
    Ok(())
}

fn check_logical_write(state: &SessionState) -> Result<(), StoreError> {
    state.validate()?;
    let bytes = serde_json::to_vec(state)
        .map_err(|_| StoreError::Corrupt("serialize materialized session state failed".into()))?;
    if bytes.len() > MAX_SESSION_BYTES {
        return Err(StoreError::Invalid(format!(
            "session state exceeds the {MAX_SESSION_BYTES} byte cap"
        )));
    }
    Ok(())
}

fn cleanup_legacy(dir: &openat::Dir) -> Result<(), StoreError> {
    let mut changed = false;
    for name in ["session.json", "session.alt.json"] {
        match dir.remove_file(name) {
            Ok(()) => changed = true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(StoreError::Io("remove migrated session slot failed".into())),
        }
    }
    if changed {
        sync_dir(dir)?;
    }
    Ok(())
}

fn remove_set(dir: &openat::Dir, set: &RecoverySet, retained: &Manifest) -> Result<(), StoreError> {
    let retained_names = [
        retained.current.snapshot.as_str(),
        retained.current.segment.as_str(),
        retained
            .previous
            .as_ref()
            .map(|set| set.snapshot.as_str())
            .unwrap_or(""),
        retained
            .previous
            .as_ref()
            .map(|set| set.segment.as_str())
            .unwrap_or(""),
    ];
    let mut changed = false;
    for name in [&set.snapshot, &set.segment] {
        if retained_names.contains(&name.as_str()) {
            continue;
        }
        match dir.remove_file(name) {
            Ok(()) => changed = true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(StoreError::Io(
                    "remove superseded session artifact failed".into(),
                ))
            }
        }
    }
    if changed {
        sync_dir(dir)?;
    }
    Ok(())
}

fn sync_dir(dir: &openat::Dir) -> Result<(), StoreError> {
    let file = dir
        .open_file(".")
        .map_err(|_| StoreError::InstalledDurabilityUnknown)?;
    if unsafe { libc::fsync(file.as_raw_fd()) } != 0 {
        return Err(StoreError::InstalledDurabilityUnknown);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
