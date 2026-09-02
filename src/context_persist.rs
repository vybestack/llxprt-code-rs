//! Phase-2 context persistence: the bulk-result digest seam, the context-store
//! state it keeps, and the `context/` artifacts it publishes. Pure moves from
//! `session.rs`; the session store keeps one-line wrappers at the old call sites.

use std::path::Path;

use serde::Serialize;

use crate::session::{
    ensure_private_subdir, open_regular_at, RoundRecord, SessionStore, StoreError,
};

/// Tool results at or above this size are bulk evidence: they are digested before the
/// transcript is persisted, and the full bytes move to the context store and vault.
pub(crate) const BULK_RESULT_BYTES: usize = 1024;
/// Redaction work budget for one bulk-result ingress transaction.
pub(crate) const INGRESS_WORK_BUDGET: usize = 1 << 20;
/// Preserved spans retained for the envelope summary, newest last.
pub(crate) const PRESERVED_SPAN_LIMIT: usize = 64;
/// Preserved spans the compact CTXDIGEST record itself may carry. The full span set still
/// persists in `context/manifest.json`, so the record only needs enough to identify the
/// preserved evidence while staying far below the pre-send request budget.
pub(crate) const DIGEST_SPAN_LIMIT: usize = 4;
/// Byte budget for the preserved-span block of one compact CTXDIGEST record.
pub(crate) const DIGEST_SPAN_BYTES: usize = 1024;

/// Lazily opened phase-2 context store with its filter registry and quiesce state.
pub(crate) struct ContextState {
    pub(crate) store: crate::context_store::store::ContextStore,
    pub(crate) filters: crate::context_ingress::filter::FilterRegistry,
    pub(crate) quiesce: Option<String>,
    pub(crate) detail: Option<String>,
    pub(crate) preserved: Vec<String>,
}

/// Durable context-store facts the CLI envelope re-reads after the run.
#[derive(Serialize)]
struct ContextManifest<'a> {
    mode: &'a str,
    pub(crate) quiesce: Option<&'a str>,
    pub(crate) detail: Option<&'a str>,
    pub(crate) preserved: &'a [String],
}

/// Deterministic in-process vault key for one session. Platform-keychain derivation is a
/// later phase; the key is stable for the session id and never leaves the process.
fn context_vault_key(session_id: &str) -> crate::context_store::vault::VaultKey {
    let mut key = [0u8; 32];
    let mut seed = crate::context_kernel::canonical::digest(b"issue39-context-vault");
    for chunk in key.chunks_mut(8) {
        seed = crate::context_kernel::canonical::chained(seed, session_id.as_bytes());
        let bytes = seed.to_le_bytes();
        chunk.copy_from_slice(&bytes[..chunk.len()]);
    }
    crate::context_store::vault::VaultKey::from(key)
}

/// Stable textual refusal for the external context store seam.
fn context_store_error(error: &crate::context_store::store::StoreError) -> String {
    match error {
        crate::context_store::store::StoreError::Spine(_) => "spine refused the write".to_string(),
        crate::context_store::store::StoreError::Vault(_) => "vault refused the write".to_string(),
        crate::context_store::store::StoreError::Blocked(_) => {
            "store mode refused the write".to_string()
        }
    }
}

/// Stable textual refusal for the ingress transaction seam.
fn ingress_error(error: &crate::context_ingress::ingress::IngressError) -> String {
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

/// Writes one context artifact through a private temporary file and an atomic rename, so
/// a torn write is never observable and an unwritable directory is detected right here.
fn write_artifact(dir: &openat::Dir, root: &Path, name: &str, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write as _;
    let temp = format!("{name}.tmp");
    let mut file = open_regular_at(
        dir,
        &temp,
        libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
        0o600,
    )
    .map_err(|error| format!("open context artifact {name} failed: {error}"))?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("write context artifact {name} failed: {error}"))?;
    drop(file);
    std::fs::rename(root.join(&temp), root.join(name))
        .map_err(|error| format!("publish context artifact {name} failed: {error}"))
}

/// Keeps the preserved-span window bounded across pre-entry compaction calls.
fn trim_preserved(state: &mut ContextState) {
    let drop = state.preserved.len().saturating_sub(PRESERVED_SPAN_LIMIT);
    state.preserved.drain(..drop);
}

/// Builds the compact CTXDIGEST record for one bulk tool result, moving the full bytes
/// through the fail-closed ingress transaction into the spine and the vault.
///
/// The record is a pure function of `(tool name, result bytes)`: the handle is a content
/// digest, never a vault slot handle, so a later re-derivation of the same call produces
/// byte-identical records and replay comparisons stay exact.
fn ingest_bulk(state: &mut ContextState, tool: &str, bytes: &[u8]) -> Result<String, String> {
    use crate::context_ingress::capture::CaptureSource;
    use crate::context_ingress::ingress::IngressTxn;
    state
        .store
        .begin_state_advancing_turn()
        .map_err(|blocked| match blocked {
            crate::context_store::store::StoreBlocked::Mode { mode } => {
                format!("store mode {mode} refused the turn")
            }
        })?;
    let handle = format!(
        "content-{:016x}",
        crate::context_kernel::canonical::digest(bytes)
    );
    let mut txn = IngressTxn::new(bytes.len(), INGRESS_WORK_BUDGET);
    txn.capture(CaptureSource::ToolResult, bytes)
        .map_err(|error| ingress_error(&error))?;
    let records = txn
        .commit(&mut state.store)
        .map_err(|error| ingress_error(&error))?;
    let segments = records
        .first()
        .ok_or_else(|| "ingress committed no records".to_string())?
        .segments
        .clone();
    let range = state
        .store
        .sanitized_append(&handle, bytes)
        .map_err(|error| context_store_error(&error))?;
    state
        .store
        .vault_put(bytes, "tool-result")
        .map_err(|error| context_store_error(&error))?;
    let digest = state
        .filters
        .digest(tool, &handle, vec![range], bytes, &segments);
    Ok(digest_record(state, tool, bytes, &handle, &digest))
}

/// The same record shape computed entirely in memory: no store, no vault, no artifacts.
///
/// Used when the store refuses the write, so a bulk result still never rides the request
/// list as raw bytes even though nothing was committed.
fn memory_digest(state: &mut ContextState, tool: &str, bytes: &[u8]) -> String {
    let handle = format!(
        "content-{:016x}",
        crate::context_kernel::canonical::digest(bytes)
    );
    let digest = state.filters.digest(tool, &handle, Vec::new(), bytes, &[]);
    digest_record(state, tool, bytes, &handle, &digest)
}

/// Renders one CTXDIGEST v1 record and folds its preserved spans into the manifest state.
fn digest_record(
    state: &mut ContextState,
    tool: &str,
    bytes: &[u8],
    handle: &str,
    digest: &crate::context_ingress::filter::Digest,
) -> String {
    let mut record = format!(
        "CTXDIGEST v1 tool={tool} bytes={} class={} rule={} vocab={} handle={handle}\n",
        bytes.len(),
        digest.class.name(),
        digest.rule_version,
        digest.vocabulary_version,
    );
    // Bounded span block: a pure function of the digest and the bytes, so re-derivation
    // (checkpoint vs finalize) still renders byte-identical records. The first spans are
    // kept whole until the byte budget cuts a span, which keeps every carried span exact
    // (never a partial line) while the full set stays durable in `context/manifest.json`.
    let mut budget = DIGEST_SPAN_BYTES;
    let mut carried = 0usize;
    for span in digest.preserved.iter() {
        if carried >= DIGEST_SPAN_LIMIT {
            break;
        }
        let text = String::from_utf8_lossy(&bytes[span.span.clone()]).into_owned();
        if carried > 0 && text.len() > budget {
            break;
        }
        budget = budget.saturating_sub(text.len());
        carried += 1;
        record.push_str(&text);
        record.push('\n');
    }
    let elided = digest.preserved.len().saturating_sub(carried);
    if elided > 0 {
        record.push_str(&format!("preserved_spans_elided={elided}\n"));
    }
    if !digest.summary.is_empty() {
        state
            .preserved
            .push(String::from_utf8_lossy(&digest.summary).into_owned());
    }
    record
}

/// Lock-poisoned fallback: a pure in-memory record with no shared state touched at all.
fn memory_quiesce_record(tool: &str, result: &str) -> String {
    let mut state = ContextState {
        store: crate::context_store::store::ContextStore::open(&context_vault_key("unavailable")),
        filters: crate::context_ingress::filter::FilterRegistry::new(),
        quiesce: Some("quiesce_unwritable".to_string()),
        detail: Some("context store lock poisoned".to_string()),
        preserved: Vec::new(),
    };
    memory_digest(&mut state, tool, result.as_bytes())
}

/// Replaces every bulk tool result with a deterministic digest record after moving the
/// full bytes through the fail-closed ingress transaction into the spine and the vault.
///
/// Results already compacted pre-entry are below the bulk threshold and are skipped, so
/// the checkpoint seam never digests the same bytes twice.
fn digest_bulk_results(state: &mut ContextState, rounds: &mut [RoundRecord]) -> Result<(), String> {
    for round in rounds.iter_mut() {
        for call in round.calls.iter_mut() {
            let bytes = call.result.as_bytes();
            if bytes.len() <= BULK_RESULT_BYTES {
                continue;
            }
            call.result = ingest_bulk(state, &call.name, bytes)?;
        }
    }
    trim_preserved(state);
    Ok(())
}

/// Digests bulk tool results and persists the phase-2 context artifacts, returning
/// the transcript the session log should store.
///
/// A context-store or artifact-write failure never fails the session transaction:
/// the run quiesces (recorded best-effort in the manifest) and keeps its committed
/// exit state, so an unwritable store degrades instead of aborting the turn.
pub(crate) fn context_exchange(
    store: &SessionStore,
    rounds: &[RoundRecord],
) -> Result<Vec<RoundRecord>, StoreError> {
    let mut guard = store
        .context
        .lock()
        .map_err(|_| StoreError::Lock("context store lock poisoned".into()))?;
    if guard.is_none() {
        let key = context_vault_key(&store.session_id);
        *guard = Some(ContextState {
            store: crate::context_store::store::ContextStore::open(&key),
            filters: crate::context_ingress::filter::FilterRegistry::new(),
            quiesce: None,
            detail: None,
            preserved: Vec::new(),
        });
    }
    let state = guard
        .as_mut()
        .ok_or_else(|| StoreError::Lock("context store missing".into()))?;
    let mut transformed = rounds.to_vec();
    if let Err(reason) = digest_bulk_results(state, &mut transformed) {
        state.quiesce = Some("quiesce_unwritable".to_string());
        state.detail = Some(reason);
    }
    if let Err(reason) = persist_context(store, state) {
        state.quiesce = Some("quiesce_unwritable".to_string());
        state.detail = Some(reason);
        record_quiesce_manifest(store, state);
    }
    Ok(transformed)
}

/// Compacts one tool result before it is recorded into the round.
///
/// Bulk results are digested here, ahead of the request list, so the request that
/// carries the result to the provider stays small and the pre-send wall guard never
/// sees raw bulk bytes. Results below the bulk threshold are returned unchanged and
/// touch no store state.
///
/// A store or artifact failure never fails the turn: the record is still compact
/// (computed in memory), the run is marked quiesce, and the marker is persisted
/// best-effort where it stays readable when only `context/` is unwritable.
pub fn compact_tool_result(store: &SessionStore, tool: &str, result: &str) -> String {
    if result.len() <= BULK_RESULT_BYTES {
        return result.to_string();
    }
    let Ok(mut guard) = store.context.lock() else {
        return memory_quiesce_record(tool, result);
    };
    if guard.is_none() {
        let key = context_vault_key(&store.session_id);
        *guard = Some(ContextState {
            store: crate::context_store::store::ContextStore::open(&key),
            filters: crate::context_ingress::filter::FilterRegistry::new(),
            quiesce: None,
            detail: None,
            preserved: Vec::new(),
        });
    }
    let Some(state) = guard.as_mut() else {
        return memory_quiesce_record(tool, result);
    };
    let record = match ingest_bulk(state, tool, result.as_bytes()) {
        Ok(record) => record,
        Err(reason) => {
            state.quiesce = Some("quiesce_unwritable".to_string());
            state.detail = Some(reason);
            memory_digest(state, tool, result.as_bytes())
        }
    };
    if let Err(reason) = persist_context(store, state) {
        state.quiesce = Some("quiesce_unwritable".to_string());
        state.detail = Some(reason);
        record_quiesce_manifest(store, state);
        record_quiesce_fallback(store, state);
    }
    trim_preserved(state);
    record
}

/// Best-effort quiesce marker beside the session, readable when only `context/` was
/// made unwritable. Same shape as `context/manifest.json`, so the envelope re-read
/// parses either location.
pub(crate) fn record_quiesce_fallback(store: &SessionStore, state: &ContextState) {
    let manifest = ContextManifest {
        mode: state.store.mode().name(),
        quiesce: state.quiesce.as_deref(),
        detail: state.detail.as_deref(),
        preserved: &state.preserved,
    };
    let Ok(bytes) = serde_json::to_vec(&manifest) else {
        return;
    };
    let _ = std::fs::write(store.session_dir.join("context-quiesce.json"), &bytes);
}

/// Opens (or creates) the private `context` subdirectory of the session.
///
/// An existing directory is never re-chmodded: an operator or harness that made the
/// store unwritable on purpose must observe the quiesce path, not be repaired past.
pub(crate) fn context_dir(store: &SessionStore) -> Result<openat::Dir, StoreError> {
    if let Ok(dir) = store.dir.sub_dir("context") {
        return Ok(dir);
    }
    ensure_private_subdir(&store.dir, "context")
}

/// Writes the sanitized spine, the vault snapshot, and the manifest under `context/`.
pub(crate) fn persist_context(store: &SessionStore, state: &ContextState) -> Result<(), String> {
    let dir =
        context_dir(store).map_err(|error| format!("open context directory failed: {error}"))?;
    let root = store.session_dir.join("context");
    write_artifact(&dir, &root, "sanitized", &state.store.spine_bytes())?;
    let vault = serde_json::to_vec(&state.store.vault_snapshot())
        .map_err(|error| format!("encode vault snapshot failed: {error}"))?;
    write_artifact(&dir, &root, "vault", &vault)?;
    let manifest = ContextManifest {
        mode: state.store.mode().name(),
        quiesce: state.quiesce.as_deref(),
        detail: state.detail.as_deref(),
        preserved: &state.preserved,
    };
    let bytes = serde_json::to_vec(&manifest)
        .map_err(|error| format!("encode context manifest failed: {error}"))?;
    write_artifact(&dir, &root, "manifest.json", &bytes)?;
    Ok(())
}

/// Best-effort in-place manifest update when an atomic replace is impossible.
pub(crate) fn record_quiesce_manifest(store: &SessionStore, state: &ContextState) {
    let manifest = ContextManifest {
        mode: state.store.mode().name(),
        quiesce: state.quiesce.as_deref(),
        detail: state.detail.as_deref(),
        preserved: &state.preserved,
    };
    let Ok(bytes) = serde_json::to_vec(&manifest) else {
        return;
    };
    let Ok(dir) = context_dir(store) else {
        return;
    };
    let Ok(mut file) = open_regular_at(
        &dir,
        "manifest.json",
        libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
        0o600,
    ) else {
        return;
    };
    use std::io::Write as _;
    let _ = file.write_all(&bytes);
    let _ = file.sync_all();
}
