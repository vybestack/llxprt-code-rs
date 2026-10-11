//! Value-free, strict-schema observations for one process invocation.
//! Missing provider facts remain null; estimates are explicitly labelled as estimates.

pub(crate) mod store_io;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Hard bound on each detail array, independent of unlimited agent budgets.
pub const MAX_RECORDS: usize = 4096;

/// Versioned terminal telemetry. Replays report no new model/tool executions.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Telemetry {
    #[schemars(range(min = 1, max = 1))]
    pub version: u8,
    pub turn_wall_ms: u64,
    pub round_count: u64,
    pub model_calls: u64,
    pub tool_calls: u64,
    pub refusal_events: u64,
    pub reservation: String,
    pub repaired_tail: bool,
    pub materialized_history_bytes: u64,
    #[schemars(length(max = 4096))]
    pub rounds: Vec<ModelCall>,
    #[schemars(length(max = 4096))]
    pub tools: Vec<ToolExecution>,
    #[schemars(length(max = 4096))]
    pub store_operations: Vec<StoreOperation>,
    pub dropped_records: u64,
    pub total_input_tokens: Option<u64>,
    pub total_output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cache_hit_ratio: Option<f64>,
}

impl Default for Telemetry {
    fn default() -> Self {
        Self {
            version: 1,
            turn_wall_ms: 0,
            round_count: 0,
            model_calls: 0,
            tool_calls: 0,
            refusal_events: 0,
            reservation: "unavailable".into(),
            repaired_tail: false,
            materialized_history_bytes: 0,
            rounds: Vec::new(),
            tools: Vec::new(),
            store_operations: Vec::new(),
            dropped_records: 0,
            total_input_tokens: None,
            total_output_tokens: None,
            total_tokens: None,
            cache_hit_ratio: None,
        }
    }
}

/// One backend invocation, including failed and cancelled calls.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelCall {
    pub round: u64,
    pub tool_calls: u64,
    pub attempt: u64,
    pub request_estimate_bytes: u64,
    pub request_token_estimate: u64,
    pub wall_ms: u64,
    pub ttft_ms: Option<u64>,
    pub wait_ms: Option<u64>,
    pub connect_queue_ms: Option<u64>,
    pub gen_ms: Option<u64>,
    pub tok_s: Option<f64>,
    pub reported_input_tokens: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub total_tokens_source: String,
    pub reasoning_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub uncached_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub finish_reason: Option<String>,
    pub outcome: String,
    pub retry: Option<Retry>,
}

/// Why a reissue was scheduled, and the monotonic wait actually observed.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Retry {
    pub error_class: String,
    pub http_status: Option<u16>,
    pub retry_after_ms: Option<u64>,
    pub backoff_ms: u64,
    pub succeeded: bool,
}

/// An executed or explicitly refused tool; output bytes precede store compaction.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolExecution {
    pub round: u64,
    pub name: String,
    pub exit_class: String,
    pub wall_ms: u64,
    pub output_bytes: u64,
    pub truncated: bool,
}

/// Actual artifact bytes consumed/published in one locked store operation.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StoreOperation {
    pub snapshot_read_bytes: u64,
    pub snapshot_written_bytes: u64,
    pub segment_read_bytes: u64,
    pub segment_written_bytes: u64,
    pub manifest_read_bytes: u64,
    pub manifest_written_bytes: u64,
}

impl Telemetry {
    /// Checked totals cover every call; absent usage or dropped detail invalidates totals.
    pub fn summarize(&mut self) {
        let sum = |field: fn(&ModelCall) -> Option<u64>| {
            if self.rounds.is_empty() || self.dropped_records != 0 {
                return None;
            }
            self.rounds
                .iter()
                .try_fold(0u64, |n, call| n.checked_add(field(call)?))
        };
        self.total_input_tokens = sum(|c| c.input_tokens);
        self.total_output_tokens = sum(|c| c.output_tokens);
        self.total_tokens = sum(|c| c.total_tokens);
        self.cache_hit_ratio = self
            .total_input_tokens
            .zip(sum(|c| c.cached_input_tokens))
            .filter(|(input, cached)| *input > 0 && cached <= input)
            .map(|(input, cached)| cached as f64 / input as f64);
    }

    pub(crate) fn push_call(&mut self, call: ModelCall) {
        self.model_calls = self.model_calls.saturating_add(1);
        self.round_count = self.round_count.max(call.round);
        if self.rounds.len() < MAX_RECORDS {
            self.rounds.push(call);
        } else {
            self.dropped_records = self.dropped_records.saturating_add(1);
        }
    }

    pub(crate) fn push_tool(&mut self, tool: ToolExecution) {
        if tool.exit_class == "budget-refused" {
            self.refusal_events = self.refusal_events.saturating_add(1);
        } else {
            self.tool_calls = self.tool_calls.saturating_add(1);
        }
        if self.tools.len() < MAX_RECORDS {
            self.tools.push(tool);
        } else {
            self.dropped_records = self.dropped_records.saturating_add(1);
        }
    }

    pub(crate) fn valid(&self) -> bool {
        self.version == 1
            && self.rounds.len() <= MAX_RECORDS
            && self.tools.len() <= MAX_RECORDS
            && self.store_operations.len() <= MAX_RECORDS
            && self.rounds.iter().all(|c| {
                c.round > 0
                    && c.attempt > 0
                    && c.tok_s.is_none_or(|v| v.is_finite() && v >= 0.0)
                    && c.finish_reason.as_ref().is_none_or(|s| s.len() <= 32)
                    && c.outcome.len() <= 32
                    && c.retry.as_ref().is_none_or(|r| r.error_class.len() <= 32)
            })
            && self
                .tools
                .iter()
                .all(|t| t.name.len() <= 256 && t.exit_class.len() <= 32)
            && self.reservation.len() <= 32
            && self
                .cache_hit_ratio
                .is_none_or(|v| v.is_finite() && (0.0..=1.0).contains(&v))
    }
}

pub(crate) fn millis(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_bounds_do_not_reset_counters_or_invent_totals() {
        let mut telemetry = Telemetry::default();
        for attempt in 1..=MAX_RECORDS as u64 + 1 {
            telemetry.push_call(ModelCall {
                round: 1,
                attempt,
                input_tokens: Some(u64::MAX),
                ..Default::default()
            });
        }
        telemetry.summarize();
        assert_eq!(telemetry.model_calls, MAX_RECORDS as u64 + 1);
        assert_eq!(telemetry.rounds.len(), MAX_RECORDS);
        assert_eq!(telemetry.dropped_records, 1);
        assert_eq!(telemetry.total_input_tokens, None);
        assert!(telemetry.valid());
        telemetry.rounds[0].tok_s = Some(f64::INFINITY);
        assert!(!telemetry.valid());
    }

    #[test]
    fn nested_unknown_fields_and_old_versions_are_rejected() {
        let mut value = serde_json::to_value(Telemetry::default()).unwrap();
        value["invented"] = serde_json::json!(1);
        assert!(serde_json::from_value::<Telemetry>(value).is_err());
        let telemetry = Telemetry {
            version: 0,
            ..Default::default()
        };
        assert!(!telemetry.valid());
        let mut value = serde_json::to_value(ModelCall::default()).unwrap();
        value["invented"] = serde_json::json!(1);
        assert!(serde_json::from_value::<ModelCall>(value).is_err());
    }
}
