//! Parameter registry classes for the Phase-4 plane.

/// Parameter classes. Calibrated updates are blocked while armed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum ParameterClass {
    SafetyInvariant,
    ProfileTunable,
    Calibrated,
    OperatorEnvelope,
}

/// One uniquely classed Phase-4 parameter.
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
    Parameter::new("queue.cycle_slots", ParameterClass::ProfileTunable, 5.0),
    Parameter::new("queue.monitor_share", ParameterClass::ProfileTunable, 1.0),
    Parameter::new("queue.max_retries", ParameterClass::ProfileTunable, 4.0),
    Parameter::new(
        "governor.per_window_quota",
        ParameterClass::Calibrated,
        4096.0,
    ),
    Parameter::new(
        "governor.per_turn_ceiling",
        ParameterClass::OperatorEnvelope,
        1024.0,
    ),
    Parameter::new("governor.alpha", ParameterClass::SafetyInvariant, 0.5),
    Parameter::new(
        "governor.quota_floor",
        ParameterClass::OperatorEnvelope,
        64.0,
    ),
    Parameter::new("pressure.arm", ParameterClass::ProfileTunable, 0.80),
    Parameter::new("pressure.disarm", ParameterClass::ProfileTunable, 0.70),
    Parameter::new("pressure.target", ParameterClass::ProfileTunable, 0.60),
    Parameter::new(
        "pressure.minimum_floor",
        ParameterClass::OperatorEnvelope,
        0.0,
    ),
    Parameter::new(
        "ladder.amortization_bar",
        ParameterClass::ProfileTunable,
        100.0,
    ),
    Parameter::new(
        "ladder.escalation_bound",
        ParameterClass::OperatorEnvelope,
        6.0,
    ),
    Parameter::new(
        "ladder.confidence_floor",
        ParameterClass::ProfileTunable,
        0.5,
    ),
    Parameter::new("monitor.sticky_cap", ParameterClass::ProfileTunable, 8.0),
    Parameter::new(
        "monitor.relaxation_windows",
        ParameterClass::ProfileTunable,
        1.0,
    ),
    Parameter::new(
        "cache.amortization_bar",
        ParameterClass::ProfileTunable,
        100.0,
    ),
    Parameter::new("cache.flush_epoch", ParameterClass::ProfileTunable, 4.0),
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

pub fn lookup(name: &str) -> Option<Parameter> {
    PARAMETERS
        .iter()
        .copied()
        .find(|parameter| parameter.name == name)
}

pub fn all() -> &'static [Parameter] {
    &PARAMETERS
}

pub fn calibratable_while_armed(name: &str) -> bool {
    lookup(name).is_some_and(|parameter| !matches!(parameter.class, ParameterClass::Calibrated))
}
