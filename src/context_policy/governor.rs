//! Admission governor: per-source/per-window quota, per-turn ceiling, and the
//! `admitted_rate <= alpha * measured_reclamation_throughput` invariant.

use std::collections::BTreeMap;
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

    source_admitted: BTreeMap<(u64, u64), u64>,
    active_window: u64,
    violations: u8,
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
            source_admitted: BTreeMap::new(),
            active_window: 0,
            violations: 0,
        }
    }

    pub fn config(&self) -> &GovernorConfig {
        &self.config
    }

    pub fn state(&self) -> &GovernorState {
        &self.state
    }

    /// Admit `bytes` from `source` in `window`.
    fn reset_window(&mut self, window: u64) {
        self.active_window = window;
        self.violations = 0;
        self.source_admitted.clear();
        self.state.turn_admitted = 0;
        self.state.window_admitted = 0;
        self.state.window_reclaimed = 0;
    }

    pub fn admit(&mut self, source: u64, bytes: u64, window: u64) -> Admission {
        if self.state.quiescing {
            return Admission::Quiesce;
        }
        if window != self.active_window {
            self.reset_window(window);
        }
        let used = self
            .source_admitted
            .get(&(source, window))
            .copied()
            .unwrap_or(0);
        if bytes > self.config.per_turn_ceiling
            || self.state.turn_admitted.saturating_add(bytes) > self.config.per_turn_ceiling
            || used.saturating_add(bytes) > self.state.quota
        {
            return Admission::Handle;
        }
        self.state.turn_admitted = self.state.turn_admitted.saturating_add(bytes);
        self.state.window_admitted = self.state.window_admitted.saturating_add(bytes);
        self.source_admitted
            .insert((source, window), used.saturating_add(bytes));
        Admission::Admit
    }

    /// Observe measured reclamation throughput for a window.
    pub fn observe_reclaim(&mut self, bytes: u64, window: u64) {
        if window != self.active_window {
            self.reset_window(window);
        }
        self.state.window_reclaimed = self.state.window_reclaimed.saturating_add(bytes);
        let violation = self.state.window_admitted as f64
            > self.config.alpha * self.state.window_reclaimed as f64;
        if !violation {
            return;
        }
        self.violations = self.violations.saturating_add(1);
        if self.violations == 1 {
            self.state.quota =
                self.config.quota_floor + (self.state.quota - self.config.quota_floor) / 2;
        } else {
            self.state.quota = self.config.quota_floor;
            self.state.at_floor = true;
            self.state.quiescing = true;
        }
    }
}
