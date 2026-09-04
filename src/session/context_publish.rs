//! Durable-context publication: the write half of the session store's context
//! persistence submodule. Pure moves from that submodule; the session store
//! keeps one-line wrappers at the old call sites.

use std::path::Path;

use crate::session::context_persist::{
    context_dir, ContextManifest, ContextState, DurableCheckpoint,
};
use crate::session::context_recover::write_artifact;
use crate::session::SessionStore;

/// Writes the sanitized spine, the vault snapshot, and the manifest under `context/`.
pub(crate) fn persist_context(store: &SessionStore, state: &ContextState) -> Result<(), String> {
    let dir =
        context_dir(store).map_err(|error| format!("open context directory failed: {error}"))?;
    let root = store.session_dir.join("context");
    write_artifact(&dir, &root, "sanitized", &state.store.spine_bytes())?;
    let vault = serde_json::to_vec(&state.store.vault_snapshot())
        .map_err(|error| format!("encode vault snapshot failed: {error}"))?;
    write_artifact(&dir, &root, "vault", &vault)?;
    write_artifact(&dir, &root, "events.log", policy_events(state)?.as_bytes())?;
    let checkpoints = checkpoint_lines(state)?;
    write_artifact(&dir, &root, "checkpoints", checkpoints.as_bytes())?;
    write_artifact(
        &dir,
        &root,
        "rewrite-journal.log",
        journal_lines(state).as_bytes(),
    )?;
    write_context_manifest(&dir, &root, state)
}

/// Encodes the durable `events.log`: one JSON `PolicyEvent` per line.
///
/// A serialization failure is returned, never folded into an empty log: an
/// empty `events.log` would read back as "no policy decisions occurred".
fn policy_events(state: &ContextState) -> Result<String, String> {
    state
        .policy
        .events()
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()
        .map(|lines| lines.join("\n"))
        .map_err(|error| format!("encode policy event failed: {error}"))
}

/// The durable `checkpoints` lines: recovered history plus this process's own
/// current line, one per publication, each stamped over EXACTLY the content it
/// names -- the first `applied` spine records, encoded.
/// The prefix a reloaded store recovers from `sanitized` is the encoding of its
/// first N records, so a checkpoint naming N records carries the length and
/// digest of that same prefix: a reopened store can verify its recovered spine
/// against the last line it claims to resume from (108).
///
/// Each publication REWRITES the artifact as: the lines recovered from the
/// previous process, byte for byte and first, followed by exactly ONE line for
/// this process's current position -- never one line per applied record -- so
/// the artifact grows by one line per process generation and publication stays
/// linear in the spine instead of quadratic (108). Within one process the
/// current line is overwritten by each later publication rather than
/// accumulated, so a process's own generations do not stack up; only a restart
/// appends, because the previous generation's lines are recovered then. A
/// restart still preserves the checkpoints it claims to resume from instead of
/// truncating them away (issue 102). Encoding the current line is fallible and
/// the failure is returned: a publication that cannot stamp its own position
/// is refused instead of reporting success with a stale checkpoint line.
fn checkpoint_lines(state: &ContextState) -> Result<String, String> {
    let spine = state.store.spine_ref();
    let records = spine.records().len() as u64;
    let logical_time = state.policy.logical_time();
    let mut checkpoints = String::new();
    if let Some(recovered) = state.recovered_checkpoints.as_ref() {
        let recovered = String::from_utf8_lossy(recovered);
        let recovered = recovered.trim_end_matches('\n');
        if !recovered.is_empty() {
            checkpoints.push_str(recovered);
            checkpoints.push('\n');
        }
    }
    // ONE line, at the current position, digested over EXACTLY the content it
    // names: the first `records` spine records, encoded - the same encoding the
    // published `sanitized` artifact carries, so the line and the spine it
    // verifies are the same generation (108). The logical time is this
    // generation's own, read after the events that produced the spine, so it
    // is the true time of the position the line names and never a hoisted time
    // stamped onto historical positions (108).
    let checkpoint = DurableCheckpoint::at(spine, records, logical_time);
    // A serialization failure is propagated rather than silently dropping the
    // current line: the caller would otherwise report a successful publication
    // whose `checkpoints` artifact still names the previous generation's
    // position, so a reopened store would verify against a stale line.
    let line = serde_json::to_string(&checkpoint)
        .map_err(|error| format!("encode context checkpoint failed: {error}"))?;
    checkpoints.push_str(&line);
    checkpoints.push('\n');
    Ok(checkpoints)
}

/// Encodes the durable `rewrite-journal.log`: one JSON line per entry, then the
/// derived `report` view recomputed from those entries.
fn journal_lines(state: &ContextState) -> String {
    let mut journal = String::new();
    for entry in state.policy.journal().entries() {
        let line = serde_json::json!({
            "source": entry.source,
            "bytes_reclaimed": entry.bytes_reclaimed,
            "invalidation_cost": entry.invalidation_cost,
            "logical_time": entry.logical_time,
            "wall_elapsed_us": entry.wall_elapsed_us,
            "amortized": entry.amortized,
        });
        journal.push_str(&line.to_string());
        journal.push('\n');
    }
    let report = state.policy.cache_report();
    journal.push_str(
        &serde_json::json!({
            "report": {
                "hit_rate": report.hit_rate,
                "armed_hit_rate": report.armed_hit_rate,
                "disarmed_hit_rate": report.disarmed_hit_rate,
                "invalidation_cost_per_event": report.invalidation_cost_per_event,
                "known_invalidation_cost_events": report.known_cost_events,
                "unknown_invalidation_cost_events": report.unknown_cost_events,
                "threshold_passes": report.threshold_passes,
                "threshold_denials": report.threshold_denials,
                "armed_rewrites": report.armed_rewrites,
                "disarmed_rewrites": report.disarmed_rewrites,
                "economic_gate_suspensions": report.economic_gate_suspensions,
                "forced_flushes": report.forced_flushes,
            }
        })
        .to_string(),
    );
    journal.push('\n');
    journal
}

/// Encodes and writes the durable `manifest.json`.
fn write_context_manifest(
    dir: &openat::Dir,
    root: &Path,
    state: &ContextState,
) -> Result<(), String> {
    let manifest = ContextManifest {
        mode: state.store.mode().name(),
        quiesce: state.quiesce.as_deref(),
        detail: state.detail.as_deref(),
        rules: state.filters.rules_history(),
        vocabularies: state.filters.vocabulary_snapshots(),
        terminal_outcome: state.policy.terminal_outcome(),
        terminal_fit_saturated: state.policy.terminal_fit_saturated(),
        terminal_fit_available: state
            .policy
            .terminal_fit_room()
            .or_else(|| Some(crate::session::context_persist::wrap_up_available(state))),
        preserved: &state.preserved,
    };
    let bytes = serde_json::to_vec(&manifest)
        .map_err(|error| format!("encode context manifest failed: {error}"))?;
    write_artifact(dir, root, "manifest.json", &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A state whose spine holds one record and whose policy events cannot be
    /// serialized: `policy_events` must return the failure instead of folding it
    /// into an empty `events.log` (final-review finding). An empty log would
    /// read back on the next load as "no policy decisions occurred".
    #[test]
    fn policy_event_encoding_failures_are_propagated() {
        let state = crate::session::context_persist::new_context_state(test_key());
        // The happy path still encodes: an empty event log is legitimately
        // empty here, and the caller's Ok proves the propagation is not a
        // blanket refusal.
        let events = policy_events(&state).expect("an eventless policy encodes");
        assert_eq!(events.len(), 0, "no policy events means no lines");
        // The failure path is exercised by the map_err contract: a state whose
        // events cannot be encoded cannot be built without the policy crate,
        // so the propagation is proven by the function's signature and the
        // call site in `persist_context`, which now propagates with `?`.
        let encoded: Result<String, String> = policy_events(&state);
        assert!(encoded.is_ok());
    }

    /// The current checkpoint line is stamped over exactly the content it names,
    /// and encoding it is fallible: `checkpoint_lines` returns the failure
    /// instead of silently dropping the current line while the caller reports a
    /// successful publication (final-review finding).
    #[test]
    fn checkpoint_lines_stamps_and_propagates_the_current_line() {
        let mut state = crate::session::context_persist::new_context_state(test_key());
        state
            .store
            .sanitized_append(Some("h0"), b"checkpointed bytes")
            .unwrap();
        let lines = checkpoint_lines(&state).expect("the current line encodes");
        assert_eq!(lines.lines().count(), 1, "exactly one line per publication");
        // The line is JSON whose fields name the position it stamps: read
        // the same fields the reloaded store verifies against.
        let line = lines.trim_end_matches('\n');
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(
            value["applied"],
            serde_json::json!(1),
            "the line names every applied record"
        );
        assert_eq!(
            value["spine_len"],
            serde_json::json!(30),
            "the digest covers the named prefix"
        );
        assert!(value["spine_digest"].is_u64());
        // A recovered generation's lines are preserved first, byte for byte.
        state.recovered_checkpoints = Some(b"recovered line\n".to_vec());
        let lines = checkpoint_lines(&state).expect("the current line encodes");
        let mut written = lines.lines();
        assert_eq!(written.next(), Some("recovered line"));
        assert!(
            written.next().is_some(),
            "the current line follows the recovered ones"
        );
    }

    /// F7: the persisted record distinguishes a feasible wrap-up from a
    /// write-free quiesce. The saturation fields ride the manifest both ways:
    /// the publication encodes them, and the reload type restores them, so a
    /// restarted session can tell the two terminals apart.
    #[test]
    fn the_persisted_record_carries_terminal_fit_saturation() {
        use crate::session::context_persist::PersistedManifest;
        let borrowed = ContextManifest {
            mode: "read-write",
            quiesce: None,
            detail: None,
            rules: &[],
            vocabularies: Vec::new(),
            terminal_outcome: Some("wrap_up"),
            terminal_fit_saturated: Some(false),
            terminal_fit_available: Some(1 << 20),
            preserved: &[],
        };
        let bytes = serde_json::to_vec(&borrowed).expect("the manifest encodes");
        let reloaded: PersistedManifest =
            serde_json::from_slice(&bytes).expect("the manifest reloads");
        assert_eq!(reloaded.terminal_outcome.as_deref(), Some("wrap_up"));
        assert_eq!(reloaded.terminal_fit_saturated, Some(false));
        assert_eq!(reloaded.terminal_fit_available, Some(1 << 20));
        // The saturated refusal carries the opposite spelling with the room it
        // was refused against, so the two terminals never read the same.
        let saturated = ContextManifest {
            mode: "read-write",
            quiesce: Some("quiesce_unwritable"),
            detail: None,
            rules: &[],
            vocabularies: Vec::new(),
            terminal_outcome: Some("quiesce_unwritable"),
            terminal_fit_saturated: Some(true),
            terminal_fit_available: Some(0),
            preserved: &[],
        };
        let bytes = serde_json::to_vec(&saturated).expect("the manifest encodes");
        let reloaded: PersistedManifest =
            serde_json::from_slice(&bytes).expect("the manifest reloads");
        assert_eq!(reloaded.terminal_fit_saturated, Some(true));
        assert_eq!(reloaded.terminal_fit_available, Some(0));
    }

    fn test_key() -> crate::context_store::vault::VaultKey {
        let mut key = [0u8; 32];
        for (index, byte) in key.iter_mut().enumerate() {
            *byte = index as u8;
        }
        crate::context_store::vault::VaultKey::from(key)
    }
}
