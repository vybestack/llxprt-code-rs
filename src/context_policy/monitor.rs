//! Read-only runtime monitor. It only proposes; it never mutates managed state.

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
            Self::Reacquisition,
            Self::RereadAfterReclaim,
            Self::FullOutputAfterDigest,
            Self::Thrash,
            Self::OverprotectiveClassification,
        ]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MonitorProposal {
    pub signal: MonitorSignal,
    pub relax_filter: bool,
    pub disarmed_windows_seen: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct MonitorCounters {
    pub reacquisition: u32,
    pub reread_after_reclaim: u32,
    pub full_output_after_digest: u32,
    pub thrash: u32,
    pub overprotective: u32,
    pub frozen: bool,
}

#[derive(Debug)]
pub struct RuntimeMonitor {
    cap: u32,
    counters: MonitorCounters,
    disarmed_windows: usize,
}

impl RuntimeMonitor {
    pub fn new(cap: u32) -> Self {
        assert!(cap > 0, "monitor sticky cap must be positive");
        Self {
            cap,
            counters: MonitorCounters::default(),
            disarmed_windows: 0,
        }
    }

    pub fn cap(&self) -> u32 {
        self.cap
    }

    /// Window boundaries own the relaxation gate. Any armed window resets it.
    pub fn begin_window(&mut self, armed: bool) {
        if armed {
            self.disarmed_windows = 0;
        } else {
            self.disarmed_windows = self.disarmed_windows.saturating_add(1);
        }
    }

    pub fn disarmed_windows(&self) -> usize {
        self.disarmed_windows
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

    pub fn observe(&mut self, signal: MonitorSignal) {
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
        *slot = slot.saturating_add(1).min(self.cap);
    }

    pub fn fail(&mut self) {
        self.counters.frozen = true;
    }

    pub fn proposals(&self) -> Vec<MonitorProposal> {
        if self.disarmed_windows == 0 {
            return Vec::new();
        }
        MonitorSignal::all()
            .into_iter()
            .map(|signal| MonitorProposal {
                signal,
                relax_filter: true,
                disarmed_windows_seen: self.disarmed_windows,
            })
            .collect()
    }
}
