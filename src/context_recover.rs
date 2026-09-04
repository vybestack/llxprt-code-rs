//! Durable-context recovery and publication: the reload half of
//! [`crate::context_persist`]. Pure moves from `context_persist.rs`; the
//! recovery seams (`SessionStore` -> live [`ContextState`]) keep their call
//! sites in `session.rs`.

use std::path::Path;

use crate::context_persist::{
    context_dir, ensure_vault_key, new_context_state, ContextState, PersistedManifest,
    CHECKPOINT_RELOAD_MAX, EVENTS_RELOAD_MAX, JOURNAL_RELOAD_MAX, MANIFEST_RELOAD_MAX,
    SPINE_RELOAD_MAX, VAULT_RELOAD_MAX,
};
use crate::session::{open_regular_at, SessionStore};

/// Reopens the durable `context/` artifacts of a previous process into a live
/// [`ContextState`]: the sanitized spine is re-framed and integrity-checked as a
/// whole (a corrupt frame is a typed error, never a silent truncation), the
/// vault snapshot restores its slots so restored handles read back, and the
/// manifest restores mode, quiesce state, and the preserved-span window.
/// Missing artifacts mean a fresh store; a corrupt artifact is a typed error
/// surfaced to the caller instead of a degraded restart (issue #102).
pub(crate) fn recover_context_state(store: &SessionStore) -> Result<ContextState, String> {
    let key = ensure_vault_key(store)?;
    let dir = match context_dir(store) {
        Ok(dir) => dir,
        Err(_) => return Ok(new_context_state(key)),
    };
    let spine_bytes = match crate::safe_file::read_artifact(&dir, "sanitized", SPINE_RELOAD_MAX) {
        Ok(bytes) => bytes,
        // Absent by kind, not by substring: only `NotFound` means no previous
        // store. An unreadable sanitized spine fails recovery instead of
        // silently returning a fresh empty store (issue 102).
        Err(crate::safe_file::ArtifactError::NotFound { .. }) => {
            return Ok(new_context_state(key));
        }
        Err(error) => return Err(format!("context spine unreadable: {error}")),
    };
    let mut state = new_context_state(key);
    state
        .store
        .load_spine_typed(&spine_bytes)
        .map_err(|error| format!("context spine corrupt: {error:?}"))?;
    recover_durable_policy_artifacts(&dir, &mut state)?;
    if let Some(outcome) = recover_manifest_artifacts(&dir, &mut state)? {
        state.policy.restore_terminal_outcome(outcome);
    }
    Ok(state)
}

/// Reloads the durable policy artifacts a previous process published -- the
/// checkpoint lines, the policy event log, and the rewrite journal -- into a
/// recovering state, and replays them into the controller so the republished
/// artifacts carry the previous process's records ahead of the new ones
/// instead of truncating them away (issue 102).
///
/// A missing artifact is a fresh session; an unreadable one is a typed error
/// surfaced to the caller, never a silent absence (issue 102).
fn recover_durable_policy_artifacts(
    dir: &openat::Dir,
    state: &mut ContextState,
) -> Result<(), String> {
    // Reload the durable checkpoint lines before anything can republish them,
    // so a restart preserves the previous process's checkpoints instead of
    // truncating the artifact to this generation's own line (issue 102).
    match crate::safe_file::read_artifact(dir, "checkpoints", CHECKPOINT_RELOAD_MAX) {
        Ok(bytes) => state.recovered_checkpoints = Some(bytes),
        // A missing checkpoint artifact is a fresh session; an unreadable one
        // fails recovery instead of being read as absence (issue 102).
        Err(crate::safe_file::ArtifactError::NotFound { .. }) => {}
        Err(error) => return Err(format!("context checkpoints unreadable: {error}")),
    }
    // Reload the policy event log: the republished `events.log` must carry the
    // previous process's policy history ahead of the new records, never
    // replace it with a fresh controller's empty log (issue 102).
    let recovered_events =
        match crate::safe_file::read_artifact(dir, "events.log", EVENTS_RELOAD_MAX) {
            Ok(bytes) => Some(load_policy_events(&bytes)?),
            Err(crate::safe_file::ArtifactError::NotFound { .. }) => None,
            Err(error) => return Err(format!("context policy events unreadable: {error}")),
        };
    // Reload the rewrite journal the same way, so the durable compaction
    // economics of the previous process survive the restart (issue 102).
    let journal =
        match crate::safe_file::read_artifact(dir, "rewrite-journal.log", JOURNAL_RELOAD_MAX) {
            Ok(bytes) => load_rewrite_journal(&bytes)?,
            Err(crate::safe_file::ArtifactError::NotFound { .. }) => RecoveredJournal::default(),
            Err(error) => return Err(format!("context rewrite journal unreadable: {error}")),
        };
    // Replay the reloaded policy history into the controller: the republished
    // artifacts then carry the previous process's records ahead of the new
    // ones, and the logical time resumes past the last reloaded record.
    state.policy.restore_history(
        recovered_events.unwrap_or_default(),
        journal.entries,
        journal.logical_time,
    );
    Ok(())
}

/// Reloads the vault snapshot and the manifest a previous process published
/// into a recovering state, and returns the persisted terminal outcome so the
/// caller restores the controller's branch instead of reopening as live
/// (issue 102).
///
/// A missing manifest is a fresh session; a missing vault snapshot is legal (no
/// quarantine ever happened). An unreadable artifact is a typed error, so
/// sealed evidence is never silently reset away and a restart never degrades
/// into a fresh store (issue 102).
fn recover_manifest_artifacts(
    dir: &openat::Dir,
    state: &mut ContextState,
) -> Result<Option<&'static str>, String> {
    match crate::safe_file::read_artifact(dir, "vault", VAULT_RELOAD_MAX) {
        Ok(bytes) => {
            let snapshot: crate::context_store::vault::VaultSnapshot =
                serde_json::from_slice(&bytes)
                    .map_err(|error| format!("context vault corrupt: {error}"))?;
            state.store.restore_vault(snapshot).map_err(|error| {
                format!(
                    "context vault refused restore: {}",
                    context_store_error(&error)
                )
            })?;
        }
        // A missing vault snapshot is legal (no quarantine ever
        // happened); an unreadable one fails recovery, so sealed
        // evidence is never silently reset away (issue 102).
        Err(crate::safe_file::ArtifactError::NotFound { .. }) => {}
        Err(error) => return Err(format!("context vault unreadable: {error}")),
    }
    // A present manifest restores mode, quiesce state, the preserved window,
    // and the filter version histories. Its absence is a fresh session;
    // an unreadable manifest fails recovery (issue 102).
    match crate::safe_file::read_artifact(dir, "manifest.json", MANIFEST_RELOAD_MAX) {
        Ok(bytes) => {
            let manifest: PersistedManifest = serde_json::from_slice(&bytes)
                .map_err(|error| format!("context manifest corrupt: {error}"))?;
            restore_manifest_into_state(&manifest, state)?;
            Ok(recover_terminal_outcome(manifest.terminal_outcome))
        }
        // A missing manifest is a fresh session (no filter history yet); an
        // unreadable manifest fails recovery instead of being read as absence
        // (issue 102).
        Err(crate::safe_file::ArtifactError::NotFound { .. }) => Ok(None),
        Err(error) => Err(format!("context manifest unreadable: {error}")),
    }
}

/// Applies one reloaded manifest to a recovering state: the filter version
/// histories (so a version named by a durable digest keeps resolving after a
/// restart), the store mode, the quiesce detail, and the preserved window.
fn restore_manifest_into_state(
    manifest: &PersistedManifest,
    state: &mut ContextState,
) -> Result<(), String> {
    // Historical filter versions reload with the store so a version named by
    // a durable digest keeps resolving after a restart (issue 102).
    state
        .filters
        .restore_histories(manifest.rules.clone())
        .map_err(|_| "context manifest filter rules are not relaxations".to_string())?;
    state
        .filters
        .restore_vocabulary_snapshots(manifest.vocabularies.clone())
        .map_err(|_| "context manifest vocabularies are not additions".to_string())?;
    if manifest.mode == "read-only" {
        state
            .store
            .set_mode(crate::context_store::store::StoreMode::ReadOnly);
    } else if manifest.mode == "unavailable" {
        state
            .store
            .set_mode(crate::context_store::store::StoreMode::Unavailable);
    }
    state.quiesce = manifest.quiesce.clone();
    state.detail = manifest.detail.clone();
    state.preserved = manifest.preserved.clone();
    Ok(())
}

/// Parses the reloaded `events.log`: one JSON `PolicyEvent` per non-empty line.
/// A malformed line is a corrupt artifact, never a silent truncation (issue 102).
fn load_policy_events(
    bytes: &[u8],
) -> Result<Vec<crate::context_policy::runtime::PolicyEvent>, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| format!("context policy events are not utf-8: {error}"))?;
    let mut events = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        // `PolicyEvent` names its operation with a `&'static str`, so the
        // deserialized value cannot borrow from `text`: parse a `serde_json::Value`
        // first (owned data), then rebuild the event by hand from its fields.
        let value: serde_json::Value = serde_json::from_str(line)
            .map_err(|error| format!("context policy event corrupt: {error}"))?;
        let event = crate::context_policy::runtime::PolicyEvent::from_json(&value)?;
        events.push(event);
    }
    Ok(events)
}

/// The reloaded form of `rewrite-journal.log`: every entry line, in file order.
///
/// The trailing `report` line is a derived view recomputed from the restored
/// entries, so only the entry lines and the logical time they reached are
/// restored.
#[derive(Default)]
struct RecoveredJournal {
    entries: Vec<crate::context_policy::cache::RewriteEntry>,
    logical_time: u64,
}

/// Parses the reloaded `rewrite-journal.log`: one JSON object per non-empty
/// line. Entry lines carry a `source`; the trailing `report` line does not and
/// is skipped. A line that names a `source` but does not deserialize as an
/// entry is a corrupt artifact (issue 102).
fn load_rewrite_journal(bytes: &[u8]) -> Result<RecoveredJournal, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| format!("context rewrite journal is not utf-8: {error}"))?;
    let mut journal = RecoveredJournal::default();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(line)
            .map_err(|error| format!("context rewrite journal corrupt: {error}"))?;
        if value.get("report").is_some() {
            continue;
        }
        let entry: crate::context_policy::cache::RewriteEntry =
            serde_json::from_value(value.clone())
                .map_err(|error| format!("context rewrite journal entry corrupt: {error}"))?;
        journal.logical_time = journal.logical_time.max(entry.logical_time);
        journal.entries.push(entry);
    }
    Ok(journal)
}

/// Maps a reloaded manifest terminal outcome back to its stable name; an
/// unknown name is left unset rather than rewritten into a made-up branch.
fn recover_terminal_outcome(outcome: Option<String>) -> Option<&'static str> {
    match outcome.as_deref() {
        Some("quiesce_unwritable") => Some("quiesce_unwritable"),
        Some("wrap_up") => Some("wrap_up"),
        Some("disarm") => Some("disarm"),
        _ => None,
    }
}

/// Stable textual refusal for the external context store seam.
pub(crate) fn context_store_error(error: &crate::context_store::store::StoreError) -> String {
    match error {
        crate::context_store::store::StoreError::Spine(_) => "spine refused the write".to_string(),
        crate::context_store::store::StoreError::Vault(_) => "vault refused the write".to_string(),
        crate::context_store::store::StoreError::Blocked(_) => {
            "store mode refused the write".to_string()
        }
    }
}

/// Stable textual refusal for the ingress transaction seam.
pub(crate) fn ingress_error(error: &crate::context_ingress::ingress::IngressError) -> String {
    match error {
        crate::context_ingress::ingress::IngressError::Capture(_) => {
            "capture refused the payload".to_string()
        }
        crate::context_ingress::ingress::IngressError::Coverage { sanitized_len } => {
            format!("segmentation did not cover {sanitized_len} sanitized bytes")
        }
        crate::context_ingress::ingress::IngressError::StoreBlocked { mode } => {
            format!("store mode {mode} refused ingress")
        }
    }
}

/// Writes one context artifact through the crash-safe publication primitive: payload to
/// a dot-prefixed temporary file inside `context/`, fsync on the payload, a
/// same-directory rename over the final name, then an fsync on the directory. A crash
/// anywhere leaves either the old or the new artifact (issue #120).
pub(crate) fn write_artifact(
    dir: &openat::Dir,
    _root: &Path,
    name: &str,
    bytes: &[u8],
) -> Result<(), String> {
    crate::safe_file::publish_artifact(dir, name, bytes, |dir, name, flags, mode| {
        open_regular_at(dir, name, flags, mode)
    })
    .map_err(|error| format!("publish context artifact {name} failed: {error}"))
}
