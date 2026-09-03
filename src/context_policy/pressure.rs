//! Dual-threshold pressure arm with hysteresis and an effective target.

/// Dual thresholds: arm at `X`, disarm at `Y < X`, fixed target `T < Y`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thresholds {
    pub arm: f64,
    pub disarm: f64,
    pub target: f64,
}

impl Thresholds {
    /// Validates `target < disarm < arm`.
    pub fn new(arm: f64, disarm: f64, target: f64) -> Result<Self, String> {
        let ordered = target < disarm;
        let upper = disarm < arm;
        if !ordered {
            return Err(String::from("target must be below disarm"));
        }
        if !upper {
            return Err(String::from("disarm must be below arm"));
        }
        Ok(Self {
            arm,
            disarm,
            target,
        })
    }
}

/// Safety tier of the pressure plane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SafetyTier {
    Disarmed,
    Armed,
}

/// Pressure tracker: hysteresis plus `max(T, M)` effective target.
#[derive(Debug)]
pub struct Pressure {
    thresholds: Thresholds,
    tier: SafetyTier,
    floor: f64,
}

impl Pressure {
    pub fn new(thresholds: Thresholds) -> Self {
        Self {
            thresholds,
            tier: SafetyTier::Disarmed,
            floor: 0.0,
        }
    }

    pub fn thresholds(&self) -> &Thresholds {
        &self.thresholds
    }

    pub fn tier(&self) -> SafetyTier {
        self.tier
    }

    /// Observe projected pressure, occupancy, and the mandatory floor `M`.
    pub fn observe(&mut self, projected: f64, occupancy: f64, minimum_floor: f64) -> SafetyTier {
        self.floor = minimum_floor;
        if self.tier == SafetyTier::Disarmed && projected >= self.thresholds.arm {
            self.tier = SafetyTier::Armed;
        } else if self.tier == SafetyTier::Armed
            && occupancy <= self.thresholds.target.max(self.floor)
        {
            self.tier = SafetyTier::Disarmed;
        }
        self.tier
    }

    /// Effective reclamation target: `max(T, M)`.
    pub fn effective_target(&self) -> f64 {
        self.thresholds.target.max(self.floor)
    }
}
