//! Admission governor for source/window quotas, turn ceilings, and reclamation rate.

use std::collections::BTreeMap;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GovernorState {
    pub quota: u64,
    pub turn_admitted: u64,
    pub window_admitted: u64,
    pub window_reclaimed: u64,
    pub violations: u32,
    pub at_floor: bool,
    pub quiescing: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    Admit,
    Handle,
    Quiesce,
}

#[derive(Debug)]
pub struct Governor {
    config: GovernorConfig,
    state: GovernorState,
    source_admitted: BTreeMap<u64, u64>,
    active_window: Option<u64>,
    floor_violation_seen: bool,
}

impl Governor {
    pub fn new(config: GovernorConfig) -> Self {
        assert!(config.per_window_quota > 0, "window quota must be positive");
        assert!(config.per_turn_ceiling > 0, "turn ceiling must be positive");
        assert!(config.quota_floor > 0, "quota floor must be positive");
        assert!(
            config.quota_floor <= config.per_window_quota,
            "quota floor exceeds initial quota"
        );
        let alpha_valid = config.alpha.is_finite() && config.alpha > 0.0 && config.alpha <= 1.0;
        assert!(alpha_valid, "alpha must be finite and in (0, 1]");
        Self {
            state: GovernorState {
                quota: config.per_window_quota,
                turn_admitted: 0,
                window_admitted: 0,
                window_reclaimed: 0,
                violations: 0,
                at_floor: config.per_window_quota == config.quota_floor,
                quiescing: false,
            },
            config,
            source_admitted: BTreeMap::new(),
            active_window: None,
            floor_violation_seen: false,
        }
    }

    pub fn config(&self) -> &GovernorConfig {
        &self.config
    }

    pub fn state(&self) -> &GovernorState {
        &self.state
    }

    /// The window the governor is currently accounting, if any admissions
    /// entered one yet.
    pub fn active_window(&self) -> Option<u64> {
        self.active_window
    }

    /// Corrects a proposal-time admission to the measured post-write size.
    ///
    /// The verdict had to be issued BEFORE the write, so the window counted
    /// the raw input bound. What actually entered the context is the record
    /// length, so the difference is refunded to the window's admission
    /// counters and counted as reclaimed working set: the window's rate
    /// predicate then judges the real keep/reclaim split, not the raw
    /// traffic bound (issue 110).
    pub fn settle_admission(&mut self, source: u64, window: u64, raw: u64, admitted: u64) {
        if self.active_window != Some(window) {
            return;
        }
        let refund = raw.saturating_sub(admitted);
        let used = self.source_admitted.entry(source).or_insert(0);
        *used = used.saturating_sub(refund);
        self.state.window_admitted = self.state.window_admitted.saturating_sub(refund);
        self.state.turn_admitted = self.state.turn_admitted.saturating_sub(refund);
        self.state.window_reclaimed = self.state.window_reclaimed.saturating_add(refund);
    }

    /// Starts a distinct model turn without changing window accounting.
    pub fn begin_turn(&mut self) {
        self.state.turn_admitted = 0;
    }

    fn enter_window(&mut self, window: u64) {
        if self.active_window == Some(window) {
            return;
        }
        if let Some(active) = self.active_window {
            assert!(
                window > active,
                "governor windows must advance monotonically"
            );
        }
        self.active_window = Some(window);
        self.source_admitted.clear();
        self.state.window_admitted = 0;
        self.state.window_reclaimed = 0;
    }

    /// Quota exhaustion always chooses the durable handle path, never truncation.
    pub fn admit(&mut self, source: u64, bytes: u64, window: u64) -> Admission {
        if self.state.quiescing {
            return Admission::Quiesce;
        }
        self.enter_window(window);
        let source_used = self.source_admitted.get(&source).copied().unwrap_or(0);
        let turn_over =
            self.state.turn_admitted.saturating_add(bytes) > self.config.per_turn_ceiling;
        let source_over = source_used.saturating_add(bytes) > self.state.quota;
        if turn_over || source_over {
            return Admission::Handle;
        }
        self.state.turn_admitted = self.state.turn_admitted.saturating_add(bytes);
        self.state.window_admitted = self.state.window_admitted.saturating_add(bytes);
        self.source_admitted
            .insert(source, source_used.saturating_add(bytes));
        Admission::Admit
    }

    pub fn observe_reclaim(&mut self, bytes: u64, window: u64) {
        self.enter_window(window);
        self.state.window_reclaimed = self.state.window_reclaimed.saturating_add(bytes);
    }

    pub fn predicate_holds(&self) -> bool {
        self.state.window_admitted as f64 <= self.config.alpha * self.state.window_reclaimed as f64
    }

    /// Close one logical window and tighten deterministically on a rate violation.
    pub fn finish_window(&mut self, window: u64) -> bool {
        assert_eq!(
            self.active_window,
            Some(window),
            "cannot finish an inactive governor window"
        );
        if self.predicate_holds() {
            self.floor_violation_seen = false;
            return true;
        }
        self.state.violations = self.state.violations.saturating_add(1);
        if self.state.quota > self.config.quota_floor {
            let distance = self.state.quota - self.config.quota_floor;
            self.state.quota = self.config.quota_floor + distance / 2;
            self.state.at_floor = self.state.quota == self.config.quota_floor;
            if self.state.at_floor {
                self.floor_violation_seen = true;
            }
        } else if self.floor_violation_seen {
            self.state.quiescing = true;
        } else {
            self.floor_violation_seen = true;
        }
        false
    }
}
