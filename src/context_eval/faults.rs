//! Fault injection points for the context evals (issue #115, GAP-M16).
//!
//! The manifest schema advertises fault names; every declared name must be implemented
//! by this harness or the manifest is rejected before a run starts. The restart and
//! crash faults kill the acceptance target's whole process group with `SIGKILL`
//! mid-invocation, so the fault is a real process death observed at a real trigger
//! point (the second scripted tool round, or a provider request in flight), never a
//! simulated one.

/// The session context store becomes unwritable mid-invocation.
pub const STORE_UNWRITABLE: &str = "store-unwritable-after-round-1";
/// Process-group `SIGKILL` after the second scripted tool round is observed.
pub const RESTART_AFTER_ROUND_2: &str = "restart-after-round-2";
/// Process-group `SIGKILL` while a provider request is in the send interval.
pub const CRASH_AT_SEND: &str = "crash-at-send";

/// Every fault name this harness implements. Manifests may only declare these.
pub const KNOWN_FAULTS: [&str; 3] = [STORE_UNWRITABLE, RESTART_AFTER_ROUND_2, CRASH_AT_SEND];

/// Reject any fault name the harness does not implement.
pub fn validate(names: &[String]) -> Result<(), String> {
    for name in names {
        if !KNOWN_FAULTS.contains(&name.as_str()) {
            return Err(format!(
                "unsupported fault {name:?} (implemented: {})",
                KNOWN_FAULTS.join(", ")
            ));
        }
    }
    Ok(())
}

/// A mid-invocation fault that kills the acceptance target's process group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidRunFault {
    /// Kill after the second scripted tool round is observed.
    Restart,
    /// Kill while a provider request is in flight.
    Crash,
}

impl MidRunFault {
    /// Stable name for reports and evidence.
    pub fn name(self) -> &'static str {
        match self {
            MidRunFault::Restart => RESTART_AFTER_ROUND_2,
            MidRunFault::Crash => CRASH_AT_SEND,
        }
    }

    /// Human-readable trigger description for the executed-fault evidence.
    pub fn trigger(self) -> &'static str {
        match self {
            MidRunFault::Restart => "second scripted tool round observed at the loopback",
            MidRunFault::Crash => "provider request observed in flight at the loopback",
        }
    }
}

/// The mid-invocation fault a scenario selected, if any.
pub fn mid_run_fault(names: &[String]) -> Option<MidRunFault> {
    if names.iter().any(|f| f == RESTART_AFTER_ROUND_2) {
        Some(MidRunFault::Restart)
    } else if names.iter().any(|f| f == CRASH_AT_SEND) {
        Some(MidRunFault::Crash)
    } else {
        None
    }
}

/// Whether the scenario asked for the unwritable-store fault.
pub fn wants_store_unwritable(names: &[String]) -> bool {
    names.iter().any(|f| f == STORE_UNWRITABLE)
}
