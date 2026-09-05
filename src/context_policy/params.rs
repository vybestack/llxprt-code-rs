//! Parameter classes and logged mutation governance for the Phase-4 plane.

use std::collections::BTreeMap;

/// Parameter classes. Calibrated updates are blocked while armed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum ParameterClass {
    SafetyInvariant,
    ProfileTunable,
    Calibrated,
    OperatorEnvelope,
    /// The name is not declared in the registry. Classed treatment must
    /// never silently default an unknown name to the most restrictive class
    /// (issue 111): the mistyped name is its own answer.
    Unknown,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateAuthority {
    ProfileLoad,
    CalibrationTxn,
    Operator,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParameterUpdate {
    pub name: &'static str,
    pub prior: f64,
    pub value: f64,
    pub logical_time: u64,
    pub authority: UpdateAuthority,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateError {
    Unknown,
    SafetyInvariant,
    WrongAuthority,
    ArmedCalibration,
    InvalidValue,
    NonMonotonicTime,
}

#[derive(Debug)]
pub struct ParameterRegistry {
    values: BTreeMap<&'static str, f64>,
    updates: Vec<ParameterUpdate>,
}

impl Default for ParameterRegistry {
    fn default() -> Self {
        Self {
            values: PARAMETERS
                .into_iter()
                .map(|parameter| (parameter.name, parameter.default))
                .collect(),
            updates: Vec::new(),
        }
    }
}

impl ParameterRegistry {
    /// The declared class of a registry name, or [`ParameterClass::Unknown`]
    /// when the name is not declared (issue 111): an unknown name is never
    /// silently mapped onto the most restrictive class.
    pub fn class_of(name: &str) -> ParameterClass {
        lookup(name)
            .map(|parameter| parameter.class)
            .unwrap_or(ParameterClass::Unknown)
    }
    pub fn value(&self, name: &str) -> Option<f64> {
        self.values.get(name).copied()
    }
    pub fn updates(&self) -> &[ParameterUpdate] {
        &self.updates
    }

    pub fn update(
        &mut self,
        name: &str,
        value: f64,
        logical_time: u64,
        authority: UpdateAuthority,
        armed: bool,
    ) -> Result<(), UpdateError> {
        let parameter = lookup(name).ok_or(UpdateError::Unknown)?;
        let expected = match parameter.class {
            ParameterClass::SafetyInvariant => return Err(UpdateError::SafetyInvariant),
            ParameterClass::ProfileTunable => UpdateAuthority::ProfileLoad,
            ParameterClass::Calibrated => UpdateAuthority::CalibrationTxn,
            ParameterClass::OperatorEnvelope => UpdateAuthority::Operator,
            // The registry is the sole source of truth (issue 111): a class
            // the registry never declared is not any operator authority, so
            // the update is refused as unknown rather than guessed.
            ParameterClass::Unknown => return Err(UpdateError::Unknown),
        };
        if authority != expected {
            return Err(UpdateError::WrongAuthority);
        }
        if armed && matches!(parameter.class, ParameterClass::Calibrated) {
            return Err(UpdateError::ArmedCalibration);
        }
        if !valid_value(parameter.name, value) {
            return Err(UpdateError::InvalidValue);
        }
        let monotonic = self
            .updates
            .last()
            .map(|entry| logical_time > entry.logical_time)
            .unwrap_or(true);
        if !monotonic {
            return Err(UpdateError::NonMonotonicTime);
        }
        let prior = self
            .values
            .insert(parameter.name, value)
            .expect("all declared parameters have values");
        self.updates.push(ParameterUpdate {
            name: parameter.name,
            prior,
            value,
            logical_time,
            authority,
        });
        Ok(())
    }
}

fn valid_value(name: &str, value: f64) -> bool {
    if !value.is_finite() || value < 0.0 {
        return false;
    }
    if matches!(
        name,
        "cache.flush_epoch"
            | "governor.per_window_quota"
            | "governor.per_turn_ceiling"
            | "governor.quota_floor"
    ) && value == 0.0
    {
        return false;
    }
    if matches!(
        name,
        "governor.alpha"
            | "pressure.arm"
            | "pressure.disarm"
            | "pressure.target"
            | "pressure.minimum_floor"
            | "ladder.confidence_floor"
    ) {
        return value <= 1.0;
    }
    true
}
