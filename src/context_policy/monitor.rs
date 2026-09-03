//! Read-only runtime monitor. It only proposes; it never mutates state.

/// Enumerated monitor signals (closed set).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum MonitorSignal {
    Reacquisition,
    RereadAfterReclaim,
    FullOutputAfterDigest,
    Thrash,
    OverprotectiveClassification,
}

impl MonitorSignal {
    pub fn all() -> [MonitorSignal; 5] {
        [
            MonitorSignal::Reacquisition,
            MonitorSignal::RereadAfterReclaim,
            MonitorSignal::FullOutputAfterDigest,
            MonitorSignal::Thrash,
            MonitorSignal::OverprotectiveClassification,
        ]
    }
}

/// A monitor proposal: read-only, relaxation-only filters allowed only after a
/// disarmed window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MonitorProposal {
    pub signal: MonitorSignal,
    pub relax_filter: bool,
    pub disarmed_windows_seen: usize,
}

/// Sticky counters with a cap; frozen when the monitor fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct MonitorCounters {
    pub reacquisition: u32,
    pub reread_after_reclaim: u32,
    pub full_output_after_digest: u32,
    pub thrash: u32,
    pub overprotective: u32,
    pub frozen: bool,
}

/// Runtime monitor with sticky, capped counters.
#[derive(Debug)]
pub struct RuntimeMonitor {
    cap: u32,
    counters: MonitorCounters,
}

impl RuntimeMonitor {
    pub fn new(cap: u32) -> Self {
        Self {
            cap,
            counters: MonitorCounters::default(),
        }
    }

    pub fn cap(&self) -> u32 {
        self.cap
    }

    pub fn counter(&self, signal: MonitorSignal) -> u32 {
        match signal {
            MonitorSignal::Reacquisition => self.counters.reacquisition,
            MonitorSignal::RereadAfterReclaim => self.counters.reread_after_reclaim,
            MonitorSignal::FullOutputAfterDigest => self.counters.full_output_after_digest,
            MonitorSignal::Thrash => self.counters.thrash,
            MonitorSignal::OverprotectiveClassification => self.counters.overprotective,
        }
    }

    pub fn counters(&self) -> &MonitorCounters {
        &self.counters
    }

    /// Observe a signal. Sticky counters cap; observations freeze when failed.
    pub fn observe(&mut self, signal: MonitorSignal, disarmed_window: bool) {
        let _ = disarmed_window;
        if self.counters.frozen {
            return;
        }
        let slot = match signal {
            MonitorSignal::Reacquisition => &mut self.counters.reacquisition,
            MonitorSignal::RereadAfterReclaim => &mut self.counters.reread_after_reclaim,
            MonitorSignal::FullOutputAfterDigest => &mut self.counters.full_output_after_digest,
            MonitorSignal::Thrash => &mut self.counters.thrash,
            MonitorSignal::OverprotectiveClassification => &mut self.counters.overprotective,
        };
        *slot = (*slot).saturating_add(1).min(self.cap);
    }

    /// Monitor failure freezes all sticky counters.
    pub fn fail(&mut self) {
        self.counters.frozen = true;
    }

    /// Relaxation-only filter proposals, permitted only after a disarmed window.
    pub fn proposals(&self, disarmed_windows: usize) -> Vec<MonitorProposal> {
        if disarmed_windows == 0 {
            return Vec::new();
        }
        MonitorSignal::all()
            .iter()
            .map(|signal| MonitorProposal {
                signal: *signal,
                relax_filter: true,
                disarmed_windows_seen: disarmed_windows,
            })
            .collect()
    }
}
