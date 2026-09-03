//! Proposal-only production controller for the context persistence seam.
//! The controller owns accounting and emits decisions; its caller owns all store writes.

use std::collections::BTreeSet;

use serde::Serialize;

use super::cache::{CacheConfig, CacheReport, RewriteEntry, RewriteJournal};
use super::governor::{Admission, Governor, GovernorConfig};
use super::monitor::RuntimeMonitor;
use super::pressure::{Pressure, SafetyTier, Thresholds};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BulkProposal {
    pub source: u64,
    pub logical_time: u64,
    pub admission: Admission,
    pub armed: bool,
    pub input_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PolicyEvent {
    pub logical_time: u64,
    pub source: u64,
    pub operation: &'static str,
    pub input_bytes: u64,
    pub admitted_bytes: u64,
    pub reclaimed_bytes: u64,
    pub armed_before: bool,
    pub armed_after: bool,
}

#[derive(Debug)]
pub struct ProposalOnlyController {
    governor: Governor,
    pressure: Pressure,
    monitor: RuntimeMonitor,
    journal: RewriteJournal,
    seen_content: BTreeSet<u64>,
    events: Vec<PolicyEvent>,
    logical_time: u64,
    terminal_outcome: Option<&'static str>,
}

impl Default for ProposalOnlyController {
    fn default() -> Self {
        Self {
            governor: Governor::new(GovernorConfig::new(4096, 4096, 1.0, 256)),
            pressure: Pressure::new(
                Thresholds::new(0.8, 0.6, 0.4).expect("fixed thresholds are valid"),
            ),
            monitor: RuntimeMonitor::new(64),
            journal: RewriteJournal::new(CacheConfig {
                amortization_bar: 64,
                flush_epoch: 8,
            }),
            seen_content: BTreeSet::new(),
            events: Vec::new(),
            logical_time: 0,
            terminal_outcome: None,
        }
    }
}

impl ProposalOnlyController {
    /// Proposes handle admission for bulk evidence without touching the context store.
    pub fn propose_bulk(
        &mut self,
        tool: &str,
        input_bytes: usize,
        projected_pressure: f64,
    ) -> BulkProposal {
        self.logical_time = self.logical_time.saturating_add(1);
        let source = crate::context_kernel::canonical::digest(tool.as_bytes());
        let tier = self
            .pressure
            .observe(projected_pressure, projected_pressure, 0.1);
        BulkProposal {
            source,
            logical_time: self.logical_time,
            admission: Admission::Handle,
            armed: tier == SafetyTier::Armed,
            input_bytes: input_bytes as u64,
        }
    }

    /// Records a completed caller-owned rewrite using measured post-rewrite pressure.
    pub fn complete_bulk(
        &mut self,
        proposal: BulkProposal,
        bytes: &[u8],
        admitted_bytes: usize,
        admitted_pressure: f64,
        wall_elapsed_us: u64,
    ) {
        let content = crate::context_kernel::canonical::digest(bytes);
        let hit = !self.seen_content.insert(content);
        self.journal.observe_access(hit, proposal.armed);
        let reclaimed = bytes.len().saturating_sub(admitted_bytes) as u64;
        self.journal.should_rewrite(reclaimed, None, proposal.armed);
        self.journal.note(RewriteEntry::new(
            proposal.source,
            reclaimed,
            None,
            proposal.logical_time,
        ));
        self.journal
            .force_flush(proposal.source, proposal.armed, wall_elapsed_us);
        self.governor.begin_turn();
        let digest_admission = self.governor.admit(
            proposal.source,
            admitted_bytes as u64,
            proposal.logical_time,
        );
        self.governor
            .observe_reclaim(reclaimed, proposal.logical_time);
        self.governor.finish_window(proposal.logical_time);
        let after = self
            .pressure
            .observe(admitted_pressure, admitted_pressure, 0.1);
        let armed_after = after == SafetyTier::Armed;
        self.monitor.begin_window(armed_after);
        self.events.push(PolicyEvent {
            logical_time: proposal.logical_time,
            source: proposal.source,
            operation: match digest_admission {
                Admission::Quiesce => "quiesce-unwritable",
                Admission::Admit | Admission::Handle => "drop-with-handle",
            },
            input_bytes: proposal.input_bytes,
            admitted_bytes: admitted_bytes as u64,
            reclaimed_bytes: reclaimed,
            armed_before: proposal.armed,
            armed_after,
        });
        self.terminal_outcome =
            if digest_admission == Admission::Quiesce || self.governor.state().quiescing {
                Some("quiesce_unwritable")
            } else if armed_after {
                None
            } else {
                Some("disarm")
            };
    }

    /// Accounts a failed caller-owned store transaction and enters a named terminal branch.
    pub fn abort_bulk(&mut self, proposal: BulkProposal) {
        let armed_after = self.pressure.observe(0.0, 0.0, 0.1) == SafetyTier::Armed;
        self.monitor.begin_window(armed_after);
        self.events.push(PolicyEvent {
            logical_time: proposal.logical_time,
            source: proposal.source,
            operation: "quiesce-unwritable",
            input_bytes: proposal.input_bytes,
            admitted_bytes: 0,
            reclaimed_bytes: 0,
            armed_before: proposal.armed,
            armed_after,
        });
        self.terminal_outcome = Some("quiesce_unwritable");
    }

    /// Records the explicit session-finalization terminal without overriding quiescence.
    pub fn wrap_up(&mut self) {
        if self.terminal_outcome == Some("quiesce_unwritable") {
            return;
        }
        self.logical_time = self.logical_time.saturating_add(1);
        let armed = self.pressure.tier() == SafetyTier::Armed;
        self.events.push(PolicyEvent {
            logical_time: self.logical_time,
            source: 0,
            operation: "wrap-up",
            input_bytes: 0,
            admitted_bytes: 0,
            reclaimed_bytes: 0,
            armed_before: armed,
            armed_after: armed,
        });
        self.terminal_outcome = Some("wrap_up");
    }

    pub fn events(&self) -> &[PolicyEvent] {
        &self.events
    }
    pub fn journal(&self) -> &RewriteJournal {
        &self.journal
    }
    pub fn cache_report(&self) -> &CacheReport {
        self.journal.report()
    }
    pub fn terminal_outcome(&self) -> Option<&'static str> {
        self.terminal_outcome
    }
    pub fn governor(&self) -> &Governor {
        &self.governor
    }
}
