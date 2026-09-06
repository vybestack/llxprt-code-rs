//! In-flight tool marker sidecar (issue #165).
//!
//! The segment log only appends per completed turn, so a headless run parked inside
//! a long local tool call (a cold `cargo test`, say) is indistinguishable from a
//! dead one: operators have nothing to stat and end up killing live sessions.
//! While a tool call executes, `tool-in-flight.json` in the session directory names
//! the tool and when it started, giving the watchdog a machine-readable busy
//! signal; it is removed the moment the call resolves, success or failure, so an
//! absent sidecar means the turn is free.
//!
//! Marker IO is best-effort like the `context-quiesce.json` precedent: a sidecar
//! failure must never fail the run, so every result is discarded.
use super::*;

/// Best-effort write of the currently executing tool and its start time.
pub(crate) fn mark(store: &SessionStore, tool: &str) {
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    let payload = serde_json::json!({ "tool": tool, "started": started });
    let _ = std::fs::create_dir_all(store.session_dir());
    let _ = std::fs::write(
        store.session_dir().join("tool-in-flight.json"),
        serde_json::to_vec(&payload).unwrap_or_default(),
    );
}

/// Best-effort removal once the call resolves; a missing marker is already clear.
pub(crate) fn clear(store: &SessionStore) {
    let _ = std::fs::remove_file(store.session_dir().join("tool-in-flight.json"));
}

#[cfg(test)]
mod tests;
