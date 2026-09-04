//! Phase-2 context persistence: the bulk-result digest seam, the context-store
//! state it keeps, and the `context/` artifacts it publishes. Pure moves from
//! `session.rs`; the session store keeps one-line wrappers at the old call sites.

use std::time::Instant;

use serde::Serialize;

use crate::context_ingress::filter::RuleVerdict;
use crate::context_ingress::ingress::IngressPayload;
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
/// Read bound for a reloaded sanitized spine (session slot cap).
pub(crate) const SPINE_RELOAD_MAX: usize = 32 << 20;
/// Read bound for a reloaded vault snapshot.
pub(crate) const VAULT_RELOAD_MAX: usize = 8 << 20;
/// Read bound for a reloaded context manifest.
pub(crate) const MANIFEST_RELOAD_MAX: usize = 1 << 20;
/// Read bound for a reloaded policy event log.
pub(crate) const EVENTS_RELOAD_MAX: usize = 8 << 20;
/// Read bound for a reloaded rewrite journal.
pub(crate) const JOURNAL_RELOAD_MAX: usize = 8 << 20;
/// Read bound for reloaded durable checkpoint lines.
pub(crate) const CHECKPOINT_RELOAD_MAX: usize = 8 << 20;

/// Lazily opened phase-2 context store with its filter registry and quiesce state.
pub(crate) struct ContextState {
    pub(crate) store: crate::context_store::store::ContextStore,
    pub(crate) filters: crate::context_ingress::filter::FilterRegistry,
    pub(crate) quiesce: Option<String>,
    pub(crate) detail: Option<String>,
    pub(crate) preserved: Vec<String>,
    pub(crate) policy: crate::context_policy::runtime::ProposalOnlyController,
    /// Durable checkpoint lines a previous process published, reloaded on
    /// recovery so a republished `checkpoints` artifact preserves them ahead
    /// of this generation's own line instead of truncating them away
    /// (issue 102 restart).
    pub(crate) recovered_checkpoints: Option<Vec<u8>>,
}

/// Durable context-store facts the CLI envelope re-reads after the run.
#[derive(Serialize)]
pub(crate) struct ContextManifest<'a> {
    pub(crate) mode: &'a str,
    pub(crate) quiesce: Option<&'a str>,
    pub(crate) detail: Option<&'a str>,
    /// Every rule and vocabulary version the session adopted, oldest first.
    /// Reloaded after a restart so a historical version keeps resolving
    /// (issue #118).
    pub(crate) rules: &'a [crate::context_ingress::filter::FilterRules],
    pub(crate) vocabularies: Vec<crate::context_ingress::filter::VocabularySnapshot>,
    pub(crate) terminal_outcome: Option<&'a str>,
    pub(crate) preserved: &'a [String],
}

/// Owned form of [`ContextManifest`] used when reloading it from disk.
#[derive(serde::Deserialize)]
pub(crate) struct PersistedManifest {
    pub(crate) mode: String,
    pub(crate) quiesce: Option<String>,
    pub(crate) detail: Option<String>,
    pub(crate) rules: Vec<crate::context_ingress::filter::FilterRules>,
    pub(crate) vocabularies: Vec<crate::context_ingress::filter::VocabularySnapshot>,
    /// Durable terminal outcome the previous process reached: recovery reads it
    /// back and restores the controller's terminal branch, so a restarted
    /// session resumes the branch it had recorded instead of reopening as live.
    pub(crate) terminal_outcome: Option<String>,
    #[serde(default)]
    pub(crate) preserved: Vec<String>,
}

/// One durable checkpoint line: what a crash-safe publication actually
/// recorded, so a reopened store can verify its recovered spine against the
/// checkpoint it claims to resume from.
///
/// The digest is computed over **exactly the content the line claims**: the
/// first `applied` spine records, encoded, which is the same prefix a reloaded
/// store recovers from those records. The old code stamped every line with the
/// digest and length of the whole final spine, so a line naming `applied = k`
/// carried a digest of content that did not exist at that checkpoint: the line
/// could not be verified against the state it describes (108).
#[derive(Serialize)]
pub(crate) struct DurableCheckpoint {
    /// Applied spine records at checkpoint time.
    pub(crate) applied: u64,
    /// Encoded byte length of the first `applied` spine records.
    pub(crate) spine_len: u64,
    /// Digest of the encoded first `applied` spine records.
    pub(crate) spine_digest: u64,
    /// Policy logical time that produced the checkpoint.
    pub(crate) logical_time: u64,
}

impl DurableCheckpoint {
    /// The checkpoint for `applied` records, digested over exactly the prefix
    /// the line names.
    pub(crate) fn at(
        spine: &crate::context_store::spine::Spine,
        applied: u64,
        logical_time: u64,
    ) -> Self {
        let covered = encoded_spine_prefix(spine, applied);
        Self {
            applied,
            spine_len: covered.len() as u64,
            spine_digest: crate::context_kernel::canonical::digest(&covered),
            logical_time,
        }
    }
}

/// Encodes exactly the first `applied` spine records: the same content a
/// reloaded store recovers from that many records, so a checkpoint's
/// `spine_len` and `spine_digest` describe content that actually exists at the
/// checkpoint's position (108).
///
/// `applied` is clamped to the record count; the result is always the encoding
/// of a whole number of frames, never a partial frame.
pub(crate) fn encoded_spine_prefix(
    spine: &crate::context_store::spine::Spine,
    applied: u64,
) -> Vec<u8> {
    let records = spine.records();
    let applied = (applied as usize).min(records.len());
    let mut out = Vec::new();
    for record in records.iter().take(applied) {
        let bytes = spine.record_bytes(record);
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(bytes);
        out.extend_from_slice(&record.content_digest.to_le_bytes());
    }
    out
}

/// Draws one 64-bit word from the std entropy pool (OS-seeded per-process keys).
fn entropy_u64() -> u64 {
    use std::hash::{BuildHasher as _, Hasher as _};
    std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
}

/// The per-session vault key, stored as a private artifact beside the session
/// state. The key is drawn from the OS entropy pool once per session, is never
/// derived from a public seed or the session id, and never leaves the process
/// except through this 0600 artifact; restarts reopen the same key so sealed
/// vault slots stay readable while no other session can derive them (issue
/// #101; platform-keychain derivation stays a later phase).
pub(crate) fn ensure_vault_key(
    store: &SessionStore,
) -> Result<crate::context_store::vault::VaultKey, String> {
    const KEY_FILE: &str = "context-vault-key";
    const KEY_LEN: usize = 32;
    match crate::safe_file::read_artifact(&store.dir, KEY_FILE, KEY_LEN) {
        Ok(bytes) => {
            if bytes.len() != KEY_LEN {
                return Err(format!(
                    "context vault key artifact is {0} bytes, want {1}",
                    bytes.len(),
                    KEY_LEN
                ));
            }
            let key = crate::context_store::vault::VaultKey::from_slice(&bytes);
            Ok(*key)
        }
        // Only a genuine absence mints a key: classified by `ErrorKind` at
        // the open call site, never by matching the rendered message. Any
        // other failure (a symlinked name, EACCES, ENOTDIR, EMFILE) is a
        // typed error surfaced to the caller, so it can never mint a fresh
        // key over an unreadable artifact and destroy every sealed slot
        // (issue 102).
        Err(crate::safe_file::ArtifactError::NotFound { .. }) => {
            let mut key = [0u8; KEY_LEN];
            for chunk in key.chunks_mut(8) {
                chunk.copy_from_slice(&entropy_u64().to_le_bytes()[..chunk.len()]);
            }
            crate::safe_file::publish_artifact(&store.dir, KEY_FILE, &key, open_regular_at)
                .map_err(|error| format!("publish context vault key failed: {error}"))?;
            Ok(crate::context_store::vault::VaultKey::from(key))
        }
        Err(error) => Err(format!("context vault key artifact unreadable: {error}")),
    }
}

pub(crate) fn new_context_state(key: crate::context_store::vault::VaultKey) -> ContextState {
    ContextState {
        store: crate::context_store::store::ContextStore::open(&key),
        filters: crate::context_ingress::filter::FilterRegistry::new(),
        quiesce: None,
        detail: None,
        preserved: Vec::new(),
        policy: crate::context_policy::runtime::ProposalOnlyController::default(),
        recovered_checkpoints: None,
    }
}

/// Keeps the preserved-span window bounded across pre-entry compaction calls.
fn trim_preserved(state: &mut ContextState) {
    let drop = state.preserved.len().saturating_sub(PRESERVED_SPAN_LIMIT);
    state.preserved.drain(..drop);
}

/// Sequences one bulk admission through the fenced executor, so the
/// transaction core is the sole writer of governed state: the row is proposed
/// from the closed registry, snapshot and generate refuse a capability that has
/// not landed, validate enforces the fit and floor preconditions, and only a
/// committed compare-and-commit releases the effect (103).
///
/// Returns the parent version the admission committed against, so the store's
/// spine position and the executor's compare-and-commit stay in step.
fn sequence_admission(
    parent_version: u64,
    bound: u64,
    budget: &crate::context_txn::budget::Budget,
    floor: u64,
) -> Result<(), String> {
    use crate::context_txn::executor::{CommitOutcome, Epoch, Executor};
    let mut executor = Executor::new(Epoch(1));
    executor
        .propose("admit-ingress", parent_version)
        .map_err(|error| executor_refusal(&error))?;
    executor
        .snapshot()
        .map_err(|error| executor_refusal(&error))?;
    executor
        .generate()
        .map_err(|error| executor_refusal(&error))?;
    executor
        .validate(bound, budget, floor, 0, 0)
        .map_err(|error| executor_refusal(&error))?;
    match executor.commit_outcome(parent_version) {
        // The parent cannot move inside this call, so a rebase verdict would
        // mean the executor itself is inconsistent with its own proposal.
        Ok(CommitOutcome::Applied) => Ok(()),
        Ok(CommitOutcome::RebaseNoOp) => {
            Err("executor reported a rebase no-op for an unmoved parent".to_string())
        }
        Err(error) => Err(executor_refusal(&error)),
    }
}

/// Region budget the context store admits bulk evidence into: the sum of the
/// session slot cap (`SPINE_RELOAD_MAX`) and the bulk work budget.
const ADMISSION_REGION_BUDGET: u64 = (SPINE_RELOAD_MAX + INGRESS_WORK_BUDGET) as u64;
/// Reclamation reserve kept out of every admission: one bulk payload's worth.
const ADMISSION_RECLAMATION_RESERVE: u64 = 1 << 20;
/// Headroom the executor must leave free so reclamation can always run.
const ADMISSION_HEADROOM: u64 = 64 << 10;

/// Stable textual refusal for the executor seam.
fn executor_refusal(error: &crate::context_txn::executor::ExecutorError) -> String {
    use crate::context_txn::executor::ExecutorError;
    match error {
        ExecutorError::CapabilityNotLanded { op } => {
            format!("operation {op} has not landed in this phase")
        }
        ExecutorError::PreconditionFailed { which } => {
            format!("admission precondition {which} failed")
        }
        ExecutorError::StaleParent { expected, actual } => format!(
            "admission parent moved from {expected} to {actual}; refusing to commit stale state"
        ),
        ExecutorError::Fenced { held, mine } => {
            format!("admission fenced: held {held}, mine {mine}")
        }
        ExecutorError::AuthorityDenied { op, by } => {
            format!("admission of {op} denied for principal {by:?}")
        }
        ExecutorError::IllegalTransition { from, to } => {
            format!("admission transition {from:?} to {to:?} is illegal")
        }
        ExecutorError::NotRebaseSafe => "admission row is not rebase-safe".to_string(),
    }
}

/// Builds the compact CTXDIGEST record for one bulk tool result, moving the full bytes
/// through the fail-closed ingress transaction into the spine and the vault.
///
/// The record is a pure function of `(tool name, result bytes)`: the handle is a content
/// digest, never a vault slot handle, so a later re-derivation of the same call produces
/// byte-identical records and replay comparisons stay exact.
fn ingest_bulk(state: &mut ContextState, tool: &str, bytes: &[u8]) -> Result<String, String> {
    let started = Instant::now();
    let pressure = normalized_pressure(bytes.len());
    let proposal = state.policy.propose_bulk(tool, bytes.len(), pressure);
    if proposal.admission == crate::context_policy::governor::Admission::Quiesce {
        state.policy.abort_bulk(proposal);
        return Err("policy governor quiesced before bulk admission".to_string());
    }
    let result = ingest_bulk_committed(state, tool, bytes);
    match result {
        Ok(record) => {
            let elapsed = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
            state.policy.complete_bulk(
                proposal,
                bytes,
                record.len(),
                normalized_pressure(record.len()),
                elapsed,
            );
            Ok(record)
        }
        Err(reason) => {
            state.policy.abort_bulk(proposal);
            Err(reason)
        }
    }
}

/// Runs one bulk-result ingestion through the store and renders the caller's
/// record for it: the admitted payload, the filter verdict that rules what the
/// caller receives, and the digested or verbatim bytes.
///
/// Every failure here is returned, never swallowed; the caller decides whether
/// the admission aborts the bulk proposal.
fn ingest_bulk_committed(
    state: &mut ContextState,
    tool: &str,
    bytes: &[u8],
) -> Result<String, String> {
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
    // The parent version is the applied spine record count, so the executor's
    // compare-and-commit is anchored to the store position it commits against.
    let parent_version = state.store.index().len() as u64;
    // Bound: the admitted payload plus its frame overhead must fit the
    // region budget net of the reclamation reserve and headroom.
    let bound = (bytes.len() as u64).saturating_add(64);
    let budget = crate::context_txn::budget::Budget {
        b: ADMISSION_REGION_BUDGET,
        r: ADMISSION_RECLAMATION_RESERVE,
        h: ADMISSION_HEADROOM,
    };
    sequence_admission(parent_version, bound, &budget, bytes.len() as u64)?;
    let mut txn = IngressTxn::new(bytes.len(), INGRESS_WORK_BUDGET);
    txn.capture(CaptureSource::ToolResult, bytes)
        .map_err(|error| ingress_error(&error))?;
    // The transaction's own exempt append (and vault reference on
    // quarantine) are the only durable results of this ingestion; the raw
    // input bytes are never written again (issue 128).
    let records = txn
        .commit(&mut state.store)
        .map_err(|error| ingress_error(&error))?;
    let record = records
        .first()
        .ok_or_else(|| "ingress committed no records".to_string())?;
    let spine = record
        .spine
        .as_ref()
        .ok_or_else(|| "ingress committed no sanitized record".to_string())?;
    let payload = IngressPayload {
        handle: spine.handle.clone(),
        ranges: vec![spine.range.clone()],
        bytes: record.sanitized.clone(),
        segments: record.segments.clone(),
    };
    // The verdict rules what the caller receives: PassVerbatim returns the
    // sanitized bytes themselves, DropBulk a drop stub, and only Digest
    // yields the CTXDIGEST substitution (issue 128).
    match state
        .filters
        .verdict(tool, &payload.segments, payload.bytes.len())
    {
        RuleVerdict::PassVerbatim => Ok(String::from_utf8_lossy(&payload.bytes).into_owned()),
        RuleVerdict::DropBulk => Ok(format!(
            "CTXDROP v1 tool={tool} bytes={} handle={}\n",
            payload.bytes.len(),
            payload.handle
        )),
        RuleVerdict::Digest => {
            let digest = state.filters.digest(
                tool,
                &payload.handle,
                payload.ranges.clone(),
                &payload.bytes,
                &payload.segments,
            );
            Ok(digest_record(state, tool, &payload, &handle, &digest))
        }
    }
}

fn normalized_pressure(bytes: usize) -> f64 {
    (bytes as f64 / BULK_RESULT_BYTES as f64).min(1.0)
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
    let payload = IngressPayload {
        handle: handle.clone(),
        ranges: Vec::new(),
        bytes: bytes.to_vec(),
        segments: Vec::new(),
    };
    if state.filters.verdict(tool, &[], bytes.len()) == RuleVerdict::PassVerbatim {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let digest = state.filters.digest(tool, &handle, Vec::new(), bytes, &[]);
    digest_record(state, tool, &payload, &handle, &digest)
}

/// Renders one CTXDIGEST v1 record and folds its preserved spans into the manifest state.
fn digest_record(
    state: &mut ContextState,
    tool: &str,
    payload: &IngressPayload,
    handle: &str,
    digest: &crate::context_ingress::filter::Digest,
) -> String {
    let mut record = format!(
        "CTXDIGEST v1 tool={tool} bytes={} class={} rule={} vocab={} handle={handle}\n",
        payload.bytes.len(),
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
        let text = String::from_utf8_lossy(&payload.bytes[span.span.clone()]).into_owned();
        // Every carried span, including the first, must fit the record byte
        // budget; a span larger than the whole budget is never carried (issue
        // #117).
        if text.len() > budget {
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
    let mut state = new_context_state([0u8; 32].into());
    state.quiesce = Some("quiesce_unwritable".to_string());
    state.detail = Some("context store lock poisoned".to_string());
    memory_digest(&mut state, tool, result.as_bytes())
}

/// Replaces every bulk tool result with a deterministic digest record after moving the
/// full bytes through the fail-closed ingress transaction into the spine and the vault.
///
/// Results already compacted pre-entry are strictly below the bulk threshold and are
/// skipped, so the checkpoint seam never digests the same bytes twice (119).
fn digest_bulk_results(state: &mut ContextState, rounds: &mut [RoundRecord]) -> Result<(), String> {
    // Strictly below the threshold skips the seam: a result of exactly
    // `BULK_RESULT_BYTES` is bulk evidence at-or-above, so it must be
    // digested like any other bulk result. This is the same at-or-above
    // comparison the filter verdict uses (`total >= rules.size_floor`),
    // so the checkpoint seam and the filter can never disagree about the
    // boundary (119).
    for round in rounds.iter_mut() {
        for call in round.calls.iter_mut() {
            let bytes = call.result.as_bytes();
            if bytes.len() < BULK_RESULT_BYTES {
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
/// The store must be writable and the artifacts must land before the session
/// log may record completion: an unavailable store blocks advancement instead
/// of being swallowed into a quiesce marker (issue #106). The run still
/// records its quiesce marker beside the session so an operator can read it.
pub(crate) fn finalize_context(store: &SessionStore) -> Result<(), StoreError> {
    let mut guard = store
        .context
        .lock()
        .map_err(|_| StoreError::Lock("context store lock poisoned".into()))?;
    let Some(state) = guard.as_mut() else {
        return Ok(());
    };
    state.policy.wrap_up();
    if let Err(reason) = persist_context(store, state) {
        state.quiesce = Some("quiesce_unwritable".to_string());
        state.detail = Some(reason.clone());
        record_quiesce_manifest(store, state);
        record_quiesce_fallback(store, state);
        return Err(StoreError::Invalid(format!(
            "context store unwritable, refusing to record completion: {reason}"
        )));
    }
    Ok(())
}

pub(crate) fn context_exchange(
    store: &SessionStore,
    rounds: &[RoundRecord],
) -> Result<Vec<RoundRecord>, StoreError> {
    let mut guard = store
        .context
        .lock()
        .map_err(|_| StoreError::Lock("context store lock poisoned".into()))?;
    if guard.is_none() {
        // Restart recovery: reopen the durable artifacts of the previous
        // process instead of silently starting from an empty store. A corrupt
        // artifact fails the exchange rather than degrading into a rewritten
        // history (issue #120).
        match recover_context_state(store) {
            Ok(state) => *guard = Some(state),
            Err(reason) => return Err(StoreError::Invalid(format!("context recovery: {reason}"))),
        }
    }
    let state = guard
        .as_mut()
        .ok_or_else(|| StoreError::Lock("context store missing".into()))?;
    let mut transformed = rounds.to_vec();
    if let Err(reason) = digest_bulk_results(state, &mut transformed) {
        state.quiesce = Some("quiesce_unwritable".to_string());
        state.detail = Some(reason.clone());
        // A blocked admission is an integrity failure: the bulk bytes never
        // entered the store, so the session must not advance (issue #106).
        return Err(StoreError::Invalid(reason));
    }
    if let Err(reason) = persist_context(store, state) {
        state.quiesce = Some("quiesce_unwritable".to_string());
        state.detail = Some(reason.clone());
        record_quiesce_manifest(store, state);
        // Persistence failed: the context state that produced this transcript is
        // not durable, so the session must not record advancement (issue #106).
        return Err(StoreError::Invalid(reason));
    }
    Ok(transformed)
}

/// Compacts one tool result before it is recorded into the round.
///
/// Bulk results are digested here, ahead of the request list, so the request that
/// carries the result to the provider stays small and the pre-send wall guard never
/// sees raw bulk bytes. Results strictly below the bulk threshold are returned
/// unchanged and touch no store state; a result exactly at the threshold is bulk
/// evidence and is digested (119).
///
/// A store or artifact failure never fails the turn: the record is still compact
/// (computed in memory), the run is marked quiesce, and the marker is persisted
/// best-effort where it stays readable when only `context/` is unwritable.
pub fn compact_tool_result(store: &SessionStore, tool: &str, result: &str) -> String {
    // Strictly below the threshold skips the seam: a result of exactly
    // `BULK_RESULT_BYTES` is bulk evidence at-or-above, so the pre-entry seam
    // digests it like any other bulk result and the request list never carries
    // raw bulk bytes. The comparison matches the filter verdict's
    // `total >= rules.size_floor` and the checkpoint seam, so all three agree
    // on the boundary (119).
    if result.len() < BULK_RESULT_BYTES {
        return result.to_string();
    }
    let Ok(mut guard) = store.context.lock() else {
        return memory_quiesce_record(tool, result);
    };
    if guard.is_none() {
        // Restart recovery as above; an unrecoverable store quiesces instead
        // of pretending the history is gone, and the compact record is still
        // computed in memory so no raw bytes ride the request list.
        match recover_context_state(store) {
            Ok(state) => *guard = Some(state),
            Err(reason) => {
                let key = match ensure_vault_key(store) {
                    Ok(key) => key,
                    Err(inner) => {
                        let mut state = new_context_state([0u8; 32].into());
                        state.quiesce = Some("quiesce_unwritable".to_string());
                        state.detail = Some(inner);
                        return memory_digest(&mut state, tool, result.as_bytes());
                    }
                };
                let mut state = new_context_state(key);
                state.quiesce = Some("quiesce_unwritable".to_string());
                state.detail = Some(reason);
                return memory_digest(&mut state, tool, result.as_bytes());
            }
        }
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

impl SessionStore {
    /// Reads one bounded deterministic page from the sanitized context spine.
    pub fn context_read_page(
        &self,
        range: std::ops::Range<u64>,
        limit: usize,
    ) -> Result<crate::context_store::spine::Page, String> {
        read_context_page(self, range, limit)
    }
}

/// Bounded deterministic range read-back for the minimum management floor.
pub fn read_context_page(
    store: &SessionStore,
    range: std::ops::Range<u64>,
    limit: usize,
) -> Result<crate::context_store::spine::Page, String> {
    if limit == 0 {
        return Err("context read limit must be positive".to_string());
    }
    let guard = store
        .context
        .lock()
        .map_err(|_| "context store lock poisoned".to_string())?;
    let state = guard
        .as_ref()
        .ok_or_else(|| "context store is not initialized".to_string())?;
    state
        .store
        .read_page(range, limit)
        .map_err(|error| context_store_error(&error))
}

/// Best-effort quiesce marker beside the session, readable when only `context/` was
/// made unwritable. Same shape as `context/manifest.json`, so the envelope re-read
/// parses either location.
pub(crate) fn record_quiesce_fallback(store: &SessionStore, state: &ContextState) {
    let manifest = ContextManifest {
        mode: state.store.mode().name(),
        quiesce: state.quiesce.as_deref(),
        detail: state.detail.as_deref(),
        rules: state.filters.rules_history(),
        vocabularies: state.filters.vocabulary_snapshots(),
        terminal_outcome: state.policy.terminal_outcome(),
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

/// Best-effort in-place manifest update when an atomic replace is impossible.
pub(crate) fn record_quiesce_manifest(store: &SessionStore, state: &ContextState) {
    let manifest = ContextManifest {
        mode: state.store.mode().name(),
        quiesce: state.quiesce.as_deref(),
        detail: state.detail.as_deref(),
        rules: state.filters.rules_history(),
        vocabularies: state.filters.vocabulary_snapshots(),
        terminal_outcome: state.policy.terminal_outcome(),
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

/// One-line wrappers so the session store keeps its old call sites while the
/// reload and publication halves live in their own modules.
pub(crate) fn persist_context(store: &SessionStore, state: &ContextState) -> Result<(), String> {
    crate::context_publish::persist_context(store, state)
}

pub(crate) fn recover_context_state(store: &SessionStore) -> Result<ContextState, String> {
    crate::context_recover::recover_context_state(store)
}

pub(crate) use crate::context_recover::{context_store_error, ingress_error};
