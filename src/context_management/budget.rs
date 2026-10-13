use super::{Consumer, ManagementError};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
static BUDGET_ID: AtomicU64 = AtomicU64::new(1);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpendConfig {
    pub total: u64,
    pub safety: u64,
    pub gate_only: u64,
    pub candidate_cost: u64,
    /// Extraction plus MAX_ASSERTIONS * worst-case leaf-page scoring.
    pub gate_cost: u64,
}
impl SpendConfig {
    fn validate(self) -> Result<(), ManagementError> {
        let ordinary = self
            .total
            .checked_sub(self.safety)
            .ok_or(ManagementError::Configuration)?;
        if self.gate_only > self.safety
            || self.candidate_cost == 0
            || self.gate_cost < crate::context_kernel::validation::GATE_SPEND_FLOOR
            || ordinary < self.candidate_cost
        {
            return Err(ManagementError::Configuration);
        }
        let possible = ordinary / self.candidate_cost;
        if possible
            .checked_mul(self.gate_cost)
            .is_none_or(|n| n > self.gate_only)
        {
            return Err(ManagementError::Configuration);
        }
        Ok(())
    }
}
/// Non-cloneable, private mint: a compactor cannot invent or duplicate gate funds.
#[derive(Debug)]
pub struct CandidatePermit {
    owner: u64,
    pub(super) id: u64,
    pub(super) turn: u64,
}
pub struct SpendBudget {
    owner: u64,
    config: SpendConfig,
    turn: u64,
    ordinary: u64,
    safety: u64,
    gate: u64,
    next: u64,
    escrow: BTreeMap<u64, u64>,
    charged: u64,
}
impl SpendBudget {
    pub fn new(config: SpendConfig, turn: u64) -> Result<Self, ManagementError> {
        config.validate()?;
        Ok(Self {
            owner: BUDGET_ID.fetch_add(1, Ordering::Relaxed),
            config,
            turn,
            ordinary: config.total - config.safety,
            safety: config.safety - config.gate_only,
            gate: config.gate_only,
            next: 1,
            escrow: BTreeMap::new(),
            charged: 0,
        })
    }
    pub fn turn(&self) -> u64 {
        self.turn
    }
    pub fn charged(&self) -> u64 {
        self.charged
    }
    pub fn gate_remaining(&self) -> u64 {
        self.gate + self.escrow.values().sum::<u64>()
    }
    pub fn config(&self) -> SpendConfig {
        self.config
    }
    /// Reserve worst-case validation before allowing the compactor to produce data.
    pub fn reserve_candidate(&mut self) -> Result<CandidatePermit, ManagementError> {
        if self.ordinary < self.config.candidate_cost || self.gate < self.config.gate_cost {
            return Err(ManagementError::Exhausted);
        }
        self.ordinary -= self.config.candidate_cost;
        self.gate -= self.config.gate_cost;
        let id = self.next;
        self.next = self.next.checked_add(1).ok_or(ManagementError::Bounds)?;
        self.escrow.insert(id, self.config.gate_cost);
        Ok(CandidatePermit {
            owner: self.owner,
            id,
            turn: self.turn,
        })
    }
    pub(super) fn charge_compactor(
        &mut self,
        permit: &CandidatePermit,
        cost: u64,
    ) -> Result<(), ManagementError> {
        self.check_permit(permit)?;
        if cost > self.config.candidate_cost {
            return Err(ManagementError::Bounds);
        }
        // The full declared allowance is charged at intent, never refunded on failure.
        self.charged += self.config.candidate_cost;
        Ok(())
    }
    pub(super) fn check_permit(&self, permit: &CandidatePermit) -> Result<(), ManagementError> {
        if permit.owner != self.owner
            || permit.turn != self.turn
            || !self.escrow.contains_key(&permit.id)
        {
            return Err(ManagementError::Turn);
        }
        Ok(())
    }
    pub(super) fn charge_gate(
        &mut self,
        permit: &CandidatePermit,
        cost: u64,
    ) -> Result<(), ManagementError> {
        self.check_permit(permit)?;
        let remaining = self
            .escrow
            .get_mut(&permit.id)
            .ok_or(ManagementError::Turn)?;
        if *remaining < cost {
            return Err(ManagementError::Exhausted);
        }
        *remaining -= cost;
        self.charged += cost;
        Ok(())
    }
    pub fn charge(&mut self, consumer: Consumer, cost: u64) -> Result<(), ManagementError> {
        if consumer.gate_only() || consumer == Consumer::Compactor {
            return Err(ManagementError::Configuration);
        }
        let ordinary = cost.min(self.ordinary);
        let safety = cost - ordinary;
        if self.safety < safety {
            return Err(ManagementError::Exhausted);
        }
        self.ordinary -= ordinary;
        self.safety -= safety;
        self.charged += cost;
        Ok(())
    }
    /// Release unused escrow only after validation/abort, not to other consumers.
    pub fn finish_candidate(&mut self, permit: CandidatePermit) -> Result<(), ManagementError> {
        self.check_permit(&permit)?;
        self.gate += self
            .escrow
            .remove(&permit.id)
            .ok_or(ManagementError::Turn)?;
        Ok(())
    }
    pub fn replenish(&mut self, provider_turn: u64) -> Result<(), ManagementError> {
        if provider_turn <= self.turn || !self.escrow.is_empty() {
            return Err(ManagementError::Turn);
        }
        self.turn = provider_turn;
        self.ordinary = self.config.total - self.config.safety;
        self.safety = self.config.safety - self.config.gate_only;
        self.gate = self.config.gate_only;
        self.charged = 0;
        Ok(())
    }
}
