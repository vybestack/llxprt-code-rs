//! Reservation heartbeat around synchronous tool execution. No lock spans the tool.
use super::{ReservedRequest, SessionStore, StoreError};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

impl SessionStore {
    /// Renew before dispatch and while a tool is running, including unbounded tools.
    /// The caller must retain any executed transcript prefix if renewal fails.
    /// Dropping the sender wakes the scoped worker on normal return or unwinding;
    /// joining it ensures no heartbeat outlives the tool or touches terminal state.
    pub(crate) fn with_tool_lease<T>(
        &self,
        reserved: &ReservedRequest,
        tool: impl FnOnce() -> T,
    ) -> Result<T, StoreError> {
        self.with_tool_lease_interval(
            reserved,
            Duration::from_secs(crate::limits::TIMEOUT_LEASE_MARGIN_SECONDS),
            tool,
        )
    }

    fn with_tool_lease_interval<T>(
        &self,
        reserved: &ReservedRequest,
        interval: Duration,
        tool: impl FnOnce() -> T,
    ) -> Result<T, StoreError> {
        self.renew_lease(reserved)?;
        std::thread::scope(|scope| {
            let (stop, stopped) = mpsc::channel::<()>();
            let heartbeat = scope.spawn(move || loop {
                match stopped.recv_timeout(interval) {
                    Err(RecvTimeoutError::Timeout) => self.renew_lease(reserved)?,
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => return Ok(()),
                }
            });
            let result = tool();
            drop(stop);
            heartbeat
                .join()
                .map_err(|_| StoreError::Lock("tool lease heartbeat panicked".into()))??;
            // Also fence a tool that returned before the next heartbeat after a reclaim.
            self.renew_lease(reserved)?;
            Ok(result)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Lifecycle, RoundRecord, SessionId};

    fn final_round(text: &str) -> Vec<RoundRecord> {
        vec![RoundRecord {
            thinking: String::new(),
            continuation: None,
            assistant: text.into(),
            calls: Vec::new(),
        }]
    }

    #[test]
    fn heartbeat_renews_during_tool_and_stops_before_finalize() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let id = SessionId::fresh();
        let store = SessionStore::load_at(&id, home.path()).unwrap();
        let observer = SessionStore::load_at(&id, home.path()).unwrap();
        let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
        let value = store
            .with_tool_lease_interval(&reserved, Duration::from_millis(5), || {
                let mut state = observer.snapshot().unwrap();
                let shortened = super::super::now_secs() + 120;
                state.branches[0].lease_expiry = shortened;
                observer.replace_snapshot(&state).unwrap();
                // Wait for an actual durable renewal while the tool is still active.
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                loop {
                    let branch = &observer.snapshot().unwrap().branches[0];
                    if branch.lease_expiry > shortened {
                        assert_eq!(branch.owner, reserved.owner);
                        assert_eq!(branch.lifecycle, Lifecycle::Pending);
                        break;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "heartbeat never renewed"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
                "tool output"
            })
            .unwrap();
        assert_eq!(value, "tool output");
        store
            .finalize(&reserved, value, &final_round(value))
            .unwrap();
        let before = serde_json::to_value(observer.snapshot().unwrap()).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(
            serde_json::to_value(observer.snapshot().unwrap()).unwrap(),
            before
        );
    }

    #[test]
    fn reclaim_during_tool_is_fenced_and_not_reported_as_expiration() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let id = SessionId::fresh();
        let store = SessionStore::load_at(&id, home.path()).unwrap();
        let other = SessionStore::load_at(&id, home.path()).unwrap();
        let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
        let error = store
            .with_tool_lease(&reserved, || {
                let mut state = other.snapshot().unwrap();
                state.branches[0].reserved_at = 1;
                state.branches[0].lease_expiry = 2;
                other.replace_snapshot(&state).unwrap();
                let reclaimed = other.start_request(None, None, "P", cwd.path()).unwrap();
                assert_ne!(reclaimed.owner, reserved.owner);
                other
                    .finalize(&reclaimed, "new owner", &final_round("new owner"))
                    .unwrap();
            })
            .unwrap_err();
        assert!(matches!(error, StoreError::Stale));
        assert_eq!(other.snapshot().unwrap().branches[0].summary, "new owner");
    }

    #[test]
    fn expired_lease_refuses_dispatch() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let store = SessionStore::load_at(&SessionId::fresh(), home.path()).unwrap();
        let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
        let mut state = store.snapshot().unwrap();
        state.branches[0].reserved_at = 1;
        state.branches[0].lease_expiry = 2;
        store.replace_snapshot(&state).unwrap();
        let error = store
            .with_tool_lease(&reserved, || panic!("must not execute"))
            .unwrap_err();
        assert!(matches!(error, StoreError::LeaseExpired { .. }));
    }

    #[test]
    fn unwinding_tool_disconnects_and_joins_heartbeat() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let store = SessionStore::load_at(&SessionId::fresh(), home.path()).unwrap();
        let reserved = store.start_request(None, None, "P", cwd.path()).unwrap();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            store.with_tool_lease(&reserved, || panic!("tool panic"))
        }));
        assert!(panic.is_err());
        store.fail(&reserved, "tool panic", &[]).unwrap();
    }
}
