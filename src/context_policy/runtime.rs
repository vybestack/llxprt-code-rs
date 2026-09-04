//! Proposal-only production controller for the context persistence seam.
//! The controller owns accounting and emits decisions; its caller owns all store writes.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

impl PolicyEvent {
    /// Rebuilds a reloaded event from an owned JSON value.
    ///
    /// `PolicyEvent` names its operation with a `&'static str`, so the derived
    /// `Deserialize` impl is only valid for `'static` input and cannot borrow a
    /// line straight out of the reloaded `events.log`. This reader takes the
    /// owned `Value` a line already parsed into and copies the fields, so the
    /// returned event borrows nothing from the reload buffer (issue 102).
    pub fn from_json(value: &serde_json::Value) -> Result<Self, String> {
        let operation = value
            .get("operation")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "context policy event missing operation".to_string())?;
        let field = |name: &str| -> Result<u64, String> {
            value
                .get(name)
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| format!("context policy event missing {name}"))
        };
        let flag = |name: &str| -> Result<bool, String> {
            value
                .get(name)
                .and_then(serde_json::Value::as_bool)
                .ok_or_else(|| format!("context policy event missing {name}"))
        };
        Ok(Self {
            logical_time: field("logical_time")?,
            source: field("source")?,
            operation: match operation {
                "quiesce-rate" => "quiesce-rate",
                "quiesce-unwritable" => "quiesce-unwritable",
                "drop-with-handle" => "drop-with-handle",
                "wrap-up" => "wrap-up",
                other => return Err(format!("context policy event unknown operation: {other}")),
            },
            input_bytes: field("input_bytes")?,
            admitted_bytes: field("admitted_bytes")?,
            reclaimed_bytes: field("reclaimed_bytes")?,
            armed_before: flag("armed_before")?,
            armed_after: flag("armed_after")?,
        })
    }
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
                Admission::Quiesce => "quiesce-rate",
                Admission::Admit | Admission::Handle => "drop-with-handle",
            },
            input_bytes: proposal.input_bytes,
            admitted_bytes: admitted_bytes as u64,
            reclaimed_bytes: reclaimed,
            armed_before: proposal.armed,
            armed_after,
        });
        // #107-1: a rate quiesce is its own terminal, distinct from an
        // unwritable store; #107-6: an ordinary disarmed completion is not a
        // terminal at all, so it records no terminal outcome.
        if digest_admission == Admission::Quiesce {
            // The quota's own refusal is the rate terminal: the caller never
            // touched the store on this path, so the write-failure branch
            // must not be recorded for it.
            self.terminal_outcome = Some("quiesce_rate");
        } else if self.governor.state().quiescing {
            // A quiescing governor is the same rate refusal carried over
            // from an earlier window, so it keeps the rate terminal too.
            self.terminal_outcome = Some("quiesce_rate");
        } else if armed_after {
            // An ordinary armed completion is not a terminal at all: the
            // episode keeps running and wrap-up can still be recorded.
            self.terminal_outcome = None;
        } else {
            // An ordinary disarmed completion records no terminal at all
            // ([r----): the disarm macrostep is a progress verdict the caller
            // renders, not a durable branch, so wrap-up is still permitted.
            self.terminal_outcome = None;
        }
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

    /// Records the explicit session-finalization terminal without overriding
    /// quiescence.
    ///
    /// #107-2: `wrap_up` refuses (and downgrades to the write-free quiesce)
    /// when the terminal fit check fails - `terminal_reserve` is fed the
    /// measured wrap-up cost against the room left in the region.
    /// #107-3: `wrap_up` refuses when the store is unwritable; the writability
    /// signal is the caller's own write-path verdict (`StoreBlocked`/
    /// `StoreError`), handed in as `writable`, never a new store API.
    /// #107-1: a rate quiesce is terminal in its own right and is not
    /// overridden, and neither is an unwritable quiesce.
    pub fn wrap_up(&mut self, wrap_up_cost: u64, available: u64, writable: bool) {
        // Only the write-failure branch is sticky here: a rate quiesce is a
        // terminal in its own right, but a later explicit wrap-up that can
        // actually be written supersedes it rather than wedging the session.
        if self.terminal_outcome == Some("quiesce_unwritable") {
            return;
        }
        if !writable {
            self.terminal_outcome = Some("quiesce_unwritable");
            return;
        }
        if !super::progress::terminal_reserve(wrap_up_cost, available) {
            self.terminal_outcome = Some("quiesce_unwritable");
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
    /// Restores the durable policy history a previous process published: the
    /// event log, the rewrite journal, and the logical time it had reached.
    ///
    /// `events.log` is reloaded so the republished log carries the previous
    /// process's records ahead of the new ones instead of replacing them; the
    /// journal entries come back so `rewrite-journal.log` likewise survives a
    /// restart; the logical time resumes past the last reloaded event so a
    /// restored event never shares a logical time with a new one.
    ///
    /// The restored events are appended after any events this process already
    /// emitted (a recovery can happen after a refused admission), and the
    /// logical time resumes past the maximum of the reloaded and local ones.
    pub fn restore_history(
        &mut self,
        events: Vec<PolicyEvent>,
        entries: Vec<RewriteEntry>,
        logical_time: u64,
    ) {
        for event in events {
            self.logical_time = self.logical_time.max(event.logical_time);
            self.events.push(event);
        }
        for entry in entries {
            self.logical_time = self.logical_time.max(entry.logical_time);
            self.journal.restore_entry(entry);
        }
        self.logical_time = self.logical_time.max(logical_time);
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
    /// Restores the terminal outcome a previous process recorded in its
    /// manifest, so a restarted session resumes the branch it had reached
    /// instead of silently reopening as a live session (issue 102 restart).
    /// Quiescence is never upgraded: a restored `quiesce_unwritable` or
    /// `quiesce_rate` stays even if a later wrap-up would have written a
    /// softer branch.
    pub fn restore_terminal_outcome(&mut self, outcome: &'static str) {
        if self.terminal_outcome == Some("quiesce_unwritable")
            || self.terminal_outcome == Some("quiesce_rate")
        {
            return;
        }
        self.terminal_outcome = Some(outcome);
    }
    /// Current policy logical time, used to stamp durable checkpoint lines.
    pub fn logical_time(&self) -> u64 {
        self.logical_time
    }
    pub fn governor(&self) -> &Governor {
        &self.governor
    }
}
