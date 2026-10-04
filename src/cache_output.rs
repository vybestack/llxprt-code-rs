//! Explicit cache-usage output ownership, independent of transcript stderr.
//! Provider counters and nullable weighted arithmetic are captured by the backend;
//! the turn publishes those observations only to an explicitly requested file.

mod records;
use crate::jsonl_sink::Sink;
pub(crate) use records::{Observation, RunCache};
use std::cell::RefCell;
use std::path::Path;

/// The latest completed call and its cumulative process-invocation accounting.
/// Fields are deliberately not constructible by external callers.
#[derive(Clone, serde::Serialize)]
pub struct Snapshot {
    pub(crate) call: Observation,
    pub(crate) run: RunCache,
}

/// Stage and OS error kind, never destination names or provider payloads.
#[derive(Debug)]
pub struct OutputError {
    pub stage: &'static str,
    pub kind: std::io::ErrorKind,
}

impl OutputError {
    pub(crate) fn at(stage: &'static str, error: std::io::Error) -> Self {
        Self {
            stage,
            kind: error.kind(),
        }
    }
}

impl std::fmt::Display for OutputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cache observation {} failed: {}", self.stage, self.kind)
    }
}
impl std::error::Error for OutputError {}

pub(crate) struct Output(RefCell<Sink>);

impl Output {
    #[cfg(test)]
    pub(crate) fn create(path: &Path) -> Result<Self, OutputError> {
        Sink::create(path)
            .map(|sink| Self(RefCell::new(sink)))
            .map_err(|error| OutputError::at("open", error))
    }

    pub(crate) fn create_external(
        path: &Path,
        config: &crate::config::ConfigHomeRoot,
    ) -> Result<Self, OutputError> {
        let parent = Sink::open_parent(path).map_err(|error| OutputError::at("open", error))?;
        crate::session::validate_diagnostic_destination(config, path, &parent)
            .map_err(|error| OutputError::at("ownership", error))?;
        Sink::create_in(path, parent)
            .map(|sink| Self(RefCell::new(sink)))
            .map_err(|error| OutputError::at("open", error))
    }

    #[cfg(test)]
    pub(crate) fn refuse_writes_for_test(&self) {
        self.0.borrow_mut().refuse_writes_for_test();
    }

    pub(crate) fn publish(&self, snapshot: &Snapshot) -> Result<(), OutputError> {
        let mut sink = self.0.borrow_mut();
        sink.write_event(&snapshot.call)
            .and_then(|()| {
                sink.write_event(&serde_json::json!({
                    "event": "prompt_cache_run", "usage": snapshot.run
                }))
            })
            .map_err(|error| OutputError::at("write", error))?;
        sink.sync_file()
            .map_err(|error| OutputError::at("sync", error))?;
        sink.sync_parent()
            .map_err(|error| OutputError::at("dir_sync", error))
    }
}
