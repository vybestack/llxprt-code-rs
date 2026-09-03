//! Parameter registry classes for the Phase-4 plane.

/// Parameter classes. Calibrated updates are blocked while armed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum ParameterClass {
    SafetyInvariant,
    ProfileTunable,
    Calibrated,
    OperatorEnvelope,
}

/// Registry rows: every named Phase-4 parameter is assigned exactly once.
#[derive(Clone, Copy, Debug)]
pub struct Parameter {
    pub name: &'static str,
    pub class: ParameterClass,
    pub default: f64,
}

impl Parameter {
    const fn new(name: &'static str, class: ParameterClass, default: f64) -> Self {
        Self {
            name,
            class,
            default,
        }
    }
}

/// The Phase-4 parameter table.
pub const PARAMETERS: [Parameter; 26] = [
    Parameter::new(
        "safety.classification_floor",
        ParameterClass::SafetyInvariant,
        1.0,
    ),
    Parameter::new(
        "safety.mandatory_reserve",
        ParameterClass::SafetyInvariant,
        1.0,
    ),
    Parameter::new("queue.cycle_slots", ParameterClass::SafetyInvariant, 5.0),
    Parameter::new("queue.monitor_share", ParameterClass::SafetyInvariant, 1.0),
    Parameter::new("queue.max_retries", ParameterClass::Calibrated, 4.0),
    Parameter::new(
        "governor.per_window_quota",
        ParameterClass::OperatorEnvelope,
        4096.0,
    ),
    Parameter::new(
        "governor.per_turn_ceiling",
        ParameterClass::OperatorEnvelope,
        1024.0,
    ),
    Parameter::new("governor.alpha", ParameterClass::Calibrated, 0.5),
    Parameter::new(
        "governor.quota_floor",
        ParameterClass::OperatorEnvelope,
        64.0,
    ),
    Parameter::new("pressure.arm", ParameterClass::Calibrated, 0.80),
    Parameter::new("pressure.disarm", ParameterClass::Calibrated, 0.70),
    Parameter::new("pressure.target", ParameterClass::Calibrated, 0.60),
    Parameter::new(
        "pressure.minimum_floor",
        ParameterClass::OperatorEnvelope,
        0.0,
    ),
    Parameter::new("ladder.amortization_bar", ParameterClass::Calibrated, 100.0),
    Parameter::new(
        "ladder.escalation_bound",
        ParameterClass::SafetyInvariant,
        6.0,
    ),
    Parameter::new("ladder.confidence_floor", ParameterClass::Calibrated, 0.5),
    Parameter::new("monitor.sticky_cap", ParameterClass::SafetyInvariant, 8.0),
    Parameter::new(
        "monitor.relaxation_windows",
        ParameterClass::SafetyInvariant,
        1.0,
    ),
    Parameter::new("cache.amortization_bar", ParameterClass::Calibrated, 100.0),
    Parameter::new("cache.flush_epoch", ParameterClass::Calibrated, 4.0),
    Parameter::new(
        "cache.invalidation_penalty",
        ParameterClass::Calibrated,
        16.0,
    ),
    Parameter::new(
        "progress.mandatory_queue_weight",
        ParameterClass::ProfileTunable,
        1.0,
    ),
    Parameter::new("progress.retry_weight", ParameterClass::ProfileTunable, 1.0),
    Parameter::new(
        "progress.terminal_reserve",
        ParameterClass::SafetyInvariant,
        1.0,
    ),
    Parameter::new(
        "profile.reclaim_aggressiveness",
        ParameterClass::ProfileTunable,
        1.0,
    ),
    Parameter::new("profile.log_verbosity", ParameterClass::ProfileTunable, 1.0),
];

/// Total lookup by exact name.
pub fn lookup(name: &str) -> Option<Parameter> {
    PARAMETERS.iter().copied().find(|p| p.name == name)
}

/// All Phase-4 parameters in registry order.
pub fn all() -> &'static [Parameter] {
    &PARAMETERS
}

/// Calibrated parameters may not be updated while armed.
pub fn calibratable_while_armed(name: &str) -> bool {
    match lookup(name) {
        Some(p) => !matches!(p.class, ParameterClass::Calibrated),
        None => false,
    }
}
