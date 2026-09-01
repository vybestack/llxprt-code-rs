//! Opt-in phase-sampled process RSS profiling.
//!
//! The profile covers only this agent process. It intentionally excludes descendants and
//! records resident mappings rather than allocator ownership.

mod sample;
mod sink;

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub use sample::Sample;

/// A profiling failure and the lifecycle stage at which it occurred.
#[derive(Clone, Debug)]
pub struct ProfilingError {
    pub stage: &'static str,
    pub message: String,
}

impl ProfilingError {
    fn new(stage: &'static str, message: impl Into<String>) -> Self {
        Self {
            stage,
            message: crate::redact::scrub_and_bound_diagnostic(&message.into()),
        }
    }
}

impl std::fmt::Display for ProfilingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.stage, self.message)
    }
}

impl std::error::Error for ProfilingError {}

/// Per-boundary delta values. Omitted values serialize as explicit JSON nulls.
#[derive(Clone, Default)]
pub struct EventData {
    pub round_index: Option<u64>,
    pub call_index: Option<u64>,
    pub request_estimate_bytes: Option<u64>,
    pub tool_schema_estimate_bytes: Option<u64>,
    pub model_reply_mapped_bytes: Option<u64>,
    pub tool_result_persisted_bytes: Option<u64>,
    pub session_slot_input_bytes: Option<u64>,
    pub session_slot_output_bytes: Option<u64>,
    pub branch_count: Option<u64>,
    pub round_count: Option<u64>,
}

#[derive(Serialize)]
struct Event<'a> {
    schema_version: u8,
    seq: u64,
    ts_unix_ms: u64,
    phase: &'a str,
    observed_after: &'a str,
    rss_bytes: u64,
    peak_rss_bytes: u64,
    new_peak: bool,
    round_index: Option<u64>,
    call_index: Option<u64>,
    executed_tool_calls: u64,
    request_estimate_bytes: Option<u64>,
    tool_schema_estimate_bytes: Option<u64>,
    model_reply_mapped_bytes: Option<u64>,
    tool_result_persisted_bytes: Option<u64>,
    session_slot_input_bytes: Option<u64>,
    session_slot_output_bytes: Option<u64>,
    turn_assistant_bytes: u64,
    turn_args_bytes: u64,
    turn_output_bytes: u64,
    branch_count: Option<u64>,
    round_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome: Option<&'a str>,
}

struct State {
    sink: sink::Sink,
    seq: u64,
    previous_phase: &'static str,
    observed_peak: Option<u64>,
    peak_phase: &'static str,
    peak_call: Option<u64>,
    executed_tool_calls: u64,
    assistant_bytes: u64,
    args_bytes: u64,
    output_bytes: u64,
    branch_count: Option<u64>,
    round_count: Option<u64>,
    finalized: bool,
}

/// Cloneable instrumentation handle. The sink descriptor and event ordering are shared.
#[derive(Clone)]
pub struct Profiler(Arc<Mutex<State>>);

/// Successful finalization details used for the one-line stderr summary.
pub struct Summary {
    pub events: u64,
    pub peak_bytes: u64,
    pub peak_phase: &'static str,
    pub peak_call: Option<u64>,
    pub path: PathBuf,
}

impl Profiler {
    /// Validate both OS sources, create the destination safely, and publish the first event.
    pub fn initialize(path: &Path) -> Result<Self, ProfilingError> {
        let initial = sample::sample().map_err(|e| ProfilingError::new("sink_init", e))?;
        let sink = sink::Sink::create(path)
            .map_err(|e| ProfilingError::new("sink_init", format!("open profile sink: {e}")))?;
        let profiler = Self(Arc::new(Mutex::new(State {
            sink,
            seq: 0,
            previous_phase: "init",
            observed_peak: None,
            peak_phase: "startup_observed",
            peak_call: None,
            executed_tool_calls: 0,
            assistant_bytes: 0,
            args_bytes: 0,
            output_bytes: 0,
            branch_count: None,
            round_count: None,
            finalized: false,
        })));
        profiler.record_sample("startup_observed", EventData::default(), None, initial)?;
        Ok(profiler)
    }

    /// Record one sampled interval boundary.
    pub fn event(&self, phase: &'static str, data: EventData) -> Result<(), ProfilingError> {
        let sample = sample::sample().map_err(|e| ProfilingError::new("sample", e))?;
        self.record_sample(phase, data, None, sample)
    }

    /// Update cumulative turn gauges before recording a boundary.
    pub fn usage(&self, calls: usize, assistant: usize, args: usize, output: usize) {
        if let Ok(mut state) = self.0.lock() {
            state.executed_tool_calls = calls as u64;
            state.assistant_bytes = assistant as u64;
            state.args_bytes = args as u64;
            state.output_bytes = output as u64;
        }
    }

    /// Write `profile_complete`, sync the file, then sync the retained parent descriptor.
    pub fn finalize(&self, outcome: &'static str) -> Result<Summary, ProfilingError> {
        let sample = sample::sample().map_err(|e| ProfilingError::new("sample", e))?;
        self.record_sample(
            "profile_complete",
            EventData::default(),
            Some(outcome),
            sample,
        )?;
        let mut state = self.lock_state("sync")?;
        state
            .sink
            .sync_file()
            .map_err(|e| ProfilingError::new("sync", format!("sync profile: {e}")))?;
        state
            .sink
            .sync_parent()
            .map_err(|e| ProfilingError::new("dir_sync", format!("sync profile directory: {e}")))?;
        state.finalized = true;
        Ok(Summary {
            events: state.seq,
            peak_bytes: state.observed_peak.unwrap_or(0),
            peak_phase: state.peak_phase,
            peak_call: state.peak_call,
            path: state.sink.path().to_path_buf(),
        })
    }

    fn record_sample(
        &self,
        phase: &'static str,
        data: EventData,
        outcome: Option<&'static str>,
        sample: Sample,
    ) -> Result<(), ProfilingError> {
        let mut state = self.lock_state("write")?;
        let new_peak = state
            .observed_peak
            .is_none_or(|peak| sample.peak_rss_bytes > peak);
        if new_peak {
            state.observed_peak = Some(sample.peak_rss_bytes);
            state.peak_phase = phase;
            state.peak_call = data.call_index;
        }
        state.seq = state
            .seq
            .checked_add(1)
            .ok_or_else(|| ProfilingError::new("write", "profile sequence overflow"))?;
        if let Some(value) = data.branch_count {
            state.branch_count = Some(value);
        }
        if let Some(value) = data.round_count {
            state.round_count = Some(value);
        }
        let event = Event {
            schema_version: 1,
            seq: state.seq,
            ts_unix_ms: now_unix_ms(),
            phase,
            observed_after: state.previous_phase,
            rss_bytes: sample.rss_bytes,
            peak_rss_bytes: sample.peak_rss_bytes,
            new_peak,
            round_index: data.round_index,
            call_index: data.call_index,
            executed_tool_calls: state.executed_tool_calls,
            request_estimate_bytes: data.request_estimate_bytes,
            tool_schema_estimate_bytes: data.tool_schema_estimate_bytes,
            model_reply_mapped_bytes: data.model_reply_mapped_bytes,
            tool_result_persisted_bytes: data.tool_result_persisted_bytes,
            session_slot_input_bytes: data.session_slot_input_bytes,
            session_slot_output_bytes: data.session_slot_output_bytes,
            turn_assistant_bytes: state.assistant_bytes,
            turn_args_bytes: state.args_bytes,
            turn_output_bytes: state.output_bytes,
            branch_count: state.branch_count,
            round_count: state.round_count,
            outcome,
        };
        state
            .sink
            .write_event(&event)
            .map_err(|e| ProfilingError::new("write", format!("write profile event: {e}")))?;
        state.previous_phase = phase;
        Ok(())
    }

    fn lock_state(
        &self,
        stage: &'static str,
    ) -> Result<std::sync::MutexGuard<'_, State>, ProfilingError> {
        self.0
            .lock()
            .map_err(|_| ProfilingError::new(stage, "profile state lock poisoned"))
    }
}

impl Summary {
    pub fn stderr_line(&self) -> String {
        let mib = self.peak_bytes / (1024 * 1024);
        let at_call = self
            .peak_call
            .map(|call| format!(" call {call}"))
            .unwrap_or_default();
        format!(
            "mem-profile: {} events, peak {} MiB first observed at {}{}, file {}",
            self.events,
            mib,
            self.peak_phase,
            at_call,
            self.path.display()
        )
    }
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

/// Mapped bytes in an already-materialized model reply.
pub fn mapped_reply_bytes(result: &crate::adapter::LlmResult) -> usize {
    result.calls.iter().fold(result.text.len(), |total, call| {
        total
            .saturating_add(call.id.len())
            .saturating_add(call.name.len())
            .saturating_add(call.args_json.len())
    })
}

#[cfg(test)]
mod tests;
