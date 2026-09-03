//! Admission governor: per-source/per-window quota, per-turn ceiling, and the
//! `admitted_rate <= alpha * measured_reclamation_throughput` invariant.

/// Governor tuning.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GovernorConfig {
    pub per_window_quota: u64,
    pub per_turn_ceiling: u64,
    pub alpha: f64,
    pub quota_floor: u64,
}

impl GovernorConfig {
    pub const fn new(
        per_window_quota: u64,
        per_turn_ceiling: u64,
        alpha: f64,
        quota_floor: u64,
    ) -> Self {
        Self {
            per_window_quota,
            per_turn_ceiling,
            alpha,
            quota_floor,
        }
    }
}

/// Observed governor state (loggable).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GovernorState {
    pub quota: u64,
    pub turn_admitted: u64,
    pub window_admitted: u64,
    pub window_reclaimed: u64,
    pub at_floor: bool,
    pub quiescing: bool,
}

/// Admission decision. Exhausted quota forces `Handle`, never truncation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    Admit,
    Handle,
    Quiesce,
}

/// Proposal-only admission governor.
#[derive(Debug)]
pub struct Governor {
    config: GovernorConfig,
    state: GovernorState,
}

impl Governor {
    pub fn new(config: GovernorConfig) -> Self {
        let quota = config.per_window_quota;
        Self {
            config,
            state: GovernorState {
                quota,
                turn_admitted: 0,
                window_admitted: 0,
                window_reclaimed: 0,
                at_floor: false,
                quiescing: false,
            },
        }
    }

    pub fn config(&self) -> &GovernorConfig {
        &self.config
    }

    pub fn state(&self) -> &GovernorState {
        &self.state
    }

    /// Admit `bytes` from `source` in `window`.
    pub fn admit(&mut self, source: u64, bytes: u64, window: u64) -> Admission {
        // RED: never enforces quota, the per-turn ceiling, or quiesce.
        let _ = (source, window);
        self.state.turn_admitted += bytes;
        self.state.window_admitted += bytes;
        Admission::Admit
    }

    /// Observe measured reclamation throughput for a window.
    pub fn observe_reclaim(&mut self, bytes: u64, window: u64) {
        // RED: records throughput but never tightens the quota or quiesces.
        let _ = window;
        self.state.window_reclaimed += bytes;
    }
}
