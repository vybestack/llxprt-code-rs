//! Acceptance of explicit, externally recorded endurance evidence, never a fast
//! fixture presented as a 48-hour run. Values are integer milliseconds/basis points.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const DURATION_FLOOR_MS: u64 = 48 * 60 * 60 * 1_000;
pub const MIN_RESTARTS: usize = 3;
pub const NON_INFERIORITY_MARGIN_BP: u32 = 500;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endurance {
    pub schema_version: u32,
    pub candidate_sha: String,
    pub protocol_digest: String,
    pub workload_digest: String,
    pub provider_class: String,
    pub restart_run: Run,
    pub matched_no_restart: Run,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub session: String,
    pub started_unix_ms: u64,
    pub finished_unix_ms: u64,
    /// Actual active observation intervals; idle time and gaps do not count.
    pub intervals: Vec<Interval>,
    pub restarts: Vec<Restart>,
    pub partial_credit_bp: u32,
    pub bounded_continuations: u64,
    pub continuations: u64,
    pub counts: Counts,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Interval {
    pub start_ms: u64,
    pub end_ms: u64,
    /// Digest of raw observations kept by the attesting operator.
    pub observations_digest: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Restart {
    pub killed_unix_ms: u64,
    pub resumed_unix_ms: u64,
    pub before_state_digest: String,
    pub after_state_digest: String,
    pub process_before: u32,
    pub process_after: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Counts {
    pub out_of_branch_wall_hits: u64,
    pub unquiesced_armed_noops: u64,
    pub infeasibility_branches: u64,
    pub wrap_ups: u64,
    pub quiesces: u64,
    pub degraded_mode_entries: u64,
    pub governor_activations: u64,
}

pub fn digest(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl Endurance {
    pub fn validate(&self, head: &str, protocol: &str) -> Result<(), String> {
        if self.schema_version != 1
            || !digest(head, 40)
            || self.candidate_sha != head
            || self.protocol_digest != protocol
            || !digest(protocol, 64)
            || !digest(&self.workload_digest, 64)
            || self.provider_class.trim().is_empty()
        {
            return Err("endurance identity/protocol mismatch".into());
        }
        self.restart_run.validate()?;
        self.matched_no_restart.validate()?;
        if self.restart_run.session == self.matched_no_restart.session
            || self.restart_run.restarts.len() < MIN_RESTARTS
            || !self.matched_no_restart.restarts.is_empty()
        {
            return Err(
                "endurance requires a distinct matched no-restart control and >=3 restarts".into(),
            );
        }
        if self
            .restart_run
            .partial_credit_bp
            .saturating_add(NON_INFERIORITY_MARGIN_BP)
            < self.matched_no_restart.partial_credit_bp
        {
            return Err(
                "endurance partial credit exceeds the predeclared non-inferiority loss".into(),
            );
        }
        Ok(())
    }
}
impl Run {
    pub fn validate(&self) -> Result<(), String> {
        if self.session.is_empty()
            || self.finished_unix_ms <= self.started_unix_ms
            || self.partial_credit_bp > 10_000
            || self.continuations == 0
            || self.bounded_continuations > self.continuations
            || u128::from(self.bounded_continuations) * 10_000
                < u128::from(self.continuations) * 9_900
            || self.counts.out_of_branch_wall_hits != 0
            || self.counts.unquiesced_armed_noops != 0
        {
            return Err("endurance release threshold failed".into());
        }
        self.validate_intervals()?;
        self.validate_restarts()
    }
    fn validate_intervals(&self) -> Result<(), String> {
        let mut previous = self.started_unix_ms;
        let mut duration = 0u64;
        for interval in &self.intervals {
            if interval.start_ms < previous
                || interval.end_ms <= interval.start_ms
                || interval.end_ms > self.finished_unix_ms
                || !digest(&interval.observations_digest, 64)
            {
                return Err("endurance interval is overlapping, unbound, or invalid".into());
            }
            duration = duration
                .checked_add(interval.end_ms - interval.start_ms)
                .ok_or("endurance duration overflow")?;
            previous = interval.end_ms;
        }
        if duration < DURATION_FLOOR_MS {
            return Err("endurance has fewer than 48 recorded active hours".into());
        }
        Ok(())
    }
    fn validate_restarts(&self) -> Result<(), String> {
        let mut kills = BTreeSet::new();
        let mut previous = self.started_unix_ms;
        for restart in &self.restarts {
            if restart.killed_unix_ms < previous
                || restart.resumed_unix_ms <= restart.killed_unix_ms
                || restart.resumed_unix_ms > self.finished_unix_ms
                || restart.process_before == 0
                || restart.process_after == 0
                || restart.process_before == restart.process_after
                || !kills.insert(restart.killed_unix_ms)
                || !digest(&restart.before_state_digest, 64)
                || restart.before_state_digest != restart.after_state_digest
            {
                return Err(
                    "endurance restart lacks distinct processes and equivalent recovered state"
                        .into(),
                );
            }
            // Downtime cannot be billed as active observation.
            if self
                .intervals
                .iter()
                .any(|i| i.start_ms < restart.resumed_unix_ms && i.end_ms > restart.killed_unix_ms)
            {
                return Err("endurance observation interval includes restart downtime".into());
            }
            previous = restart.resumed_unix_ms;
        }
        Ok(())
    }
}
