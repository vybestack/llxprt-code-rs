//! Dual-threshold pressure arm with hysteresis and an effective target.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thresholds {
    pub arm: f64,
    pub disarm: f64,
    pub target: f64,
}

impl Thresholds {
    pub fn new(arm: f64, disarm: f64, target: f64) -> Result<Self, String> {
        let finite = arm.is_finite() && disarm.is_finite() && target.is_finite();
        if !finite {
            return Err(String::from("pressure thresholds must be finite"));
        }
        let in_range = (0.0..=1.0).contains(&arm)
            && (0.0..=1.0).contains(&disarm)
            && (0.0..=1.0).contains(&target);
        if !in_range {
            return Err(String::from("pressure thresholds must be in [0, 1]"));
        }
        if target >= disarm {
            return Err(String::from("target must be below disarm"));
        }
        if disarm >= arm {
            return Err(String::from("disarm must be below arm"));
        }
        Ok(Self {
            arm,
            disarm,
            target,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SafetyTier {
    Disarmed,
    Armed,
}

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

    pub fn observe(&mut self, projected: f64, occupancy: f64, minimum_floor: f64) -> SafetyTier {
        let finite = projected.is_finite() && occupancy.is_finite() && minimum_floor.is_finite();
        assert!(finite, "pressure observations must be finite");
        let floor_valid = (0.0..=1.0).contains(&minimum_floor);
        assert!(floor_valid, "minimum floor must be in [0, 1]");
        self.floor = minimum_floor;
        if self.tier == SafetyTier::Disarmed && projected >= self.thresholds.arm {
            self.tier = SafetyTier::Armed;
        } else if self.tier == SafetyTier::Armed && occupancy <= self.effective_target() {
            self.tier = SafetyTier::Disarmed;
        }
        self.tier
    }

    pub fn effective_target(&self) -> f64 {
        self.thresholds.target.max(self.floor)
    }
}
