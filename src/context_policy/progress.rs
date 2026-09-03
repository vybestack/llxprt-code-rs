//! Lexicographic progress and adversarial reachable-state episode verification.

use crate::context_policy::queue::find_admissible;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TerminalOutcome {
    Disarmed,
    WrapUp,
    Quiesced,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ProgressState {
    pub psi: u64,
    pub retries_remaining: u32,
}

impl ProgressState {
    pub const fn new(psi: u64, retries_remaining: u32) -> Self {
        Self {
            psi,
            retries_remaining,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Macrostep {
    NoOp,
    Reclaim,
    WrapUp,
    Quiesce,
    Disarm,
}

pub fn lexicographically_decreases(before: ProgressState, after: ProgressState) -> bool {
    after.psi < before.psi
        || (after.psi == before.psi && after.retries_remaining < before.retries_remaining)
}

pub fn terminal_reserve(wrap_up_cost: u64, available: u64) -> bool {
    wrap_up_cost <= available
}

pub fn next_action(
    current: ProgressState,
    armed: bool,
    terminal: Option<TerminalOutcome>,
) -> Macrostep {
    match terminal {
        Some(TerminalOutcome::Disarmed) => Macrostep::Disarm,
        Some(TerminalOutcome::WrapUp) => Macrostep::WrapUp,
        Some(TerminalOutcome::Quiesced) => Macrostep::Quiesce,
        None if !armed => Macrostep::Disarm,
        None if current.psi == 0 => Macrostep::Disarm,
        None if current.retries_remaining == 0 => Macrostep::Quiesce,
        None => Macrostep::Reclaim,
    }
}

/// Product axes from the design's adversarial reachable-state generator.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DegradationModes(u8);

impl DegradationModes {
    pub const COUNT: u8 = 1 << 7;

    pub const fn from_bits(bits: u8) -> Self {
        Self(bits & (Self::COUNT - 1))
    }

    pub const fn obligation_saturated(self) -> bool {
        self.0 & 1 != 0
    }
    pub const fn floored_tail(self) -> bool {
        self.0 & 2 != 0
    }
    pub const fn scorer_outage(self) -> bool {
        self.0 & 4 != 0
    }
    pub const fn placeholder_intolerant(self) -> bool {
        self.0 & 8 != 0
    }
    pub const fn spend_exhausted(self) -> bool {
        self.0 & 16 != 0
    }
    pub const fn pin_saturated(self) -> bool {
        self.0 & 32 != 0
    }
    pub const fn storage_read_only(self) -> bool {
        self.0 & 64 != 0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AdversarialReport {
    pub reachable_states: u64,
    pub transitions: u64,
    pub out_of_branch_wall_hits: u64,
    pub armed_noops: u64,
    pub unterminated_episodes: u64,
    pub max_macrosteps: u32,
}

fn degraded_action(state: ProgressState, modes: DegradationModes) -> Macrostep {
    if modes.storage_read_only() {
        return Macrostep::Quiesce;
    }
    if modes.spend_exhausted() && state.psi <= 1 {
        return Macrostep::WrapUp;
    }
    next_action(state, true, None)
}

fn registered_operation(action: Macrostep) -> Option<&'static str> {
    match action {
        Macrostep::Reclaim => {
            find_admissible(usize::MAX, |row| row.reclamation && row.deterministic)
                .map(|row| row.name)
        }
        Macrostep::WrapUp => Some("wrap-up"),
        Macrostep::Quiesce => Some("quiesce-unwritable"),
        Macrostep::Disarm => Some("disarm"),
        Macrostep::NoOp => None,
    }
}

/// Exhaust the product of degradation modes using registered operation sequences.
pub fn verify_adversarial_reachable_states(
    max_psi: u64,
    max_retries: u32,
    macrostep_bound: u32,
) -> AdversarialReport {
    let mut report = AdversarialReport::default();
    for bits in 0..DegradationModes::COUNT {
        let modes = DegradationModes::from_bits(bits);
        for initial_psi in 0..=max_psi {
            for initial_retries in 0..=max_retries {
                report.reachable_states = report.reachable_states.saturating_add(1);
                let mut state = ProgressState::new(initial_psi, initial_retries);
                let mut terminated = false;
                for step in 0..macrostep_bound {
                    let action = degraded_action(state, modes);
                    if matches!(action, Macrostep::NoOp) {
                        report.armed_noops = report.armed_noops.saturating_add(1);
                        break;
                    }
                    let Some(operation) = registered_operation(action) else {
                        report.out_of_branch_wall_hits =
                            report.out_of_branch_wall_hits.saturating_add(1);
                        break;
                    };
                    let registered = crate::context_txn::operation::find(operation).is_some();
                    if !registered {
                        report.out_of_branch_wall_hits =
                            report.out_of_branch_wall_hits.saturating_add(1);
                        break;
                    }
                    report.transitions = report.transitions.saturating_add(1);
                    match action {
                        Macrostep::Reclaim => {
                            let before = state;
                            state.psi = state.psi.saturating_sub(1);
                            state.retries_remaining = state.retries_remaining.saturating_sub(1);
                            if !lexicographically_decreases(before, state) {
                                report.armed_noops = report.armed_noops.saturating_add(1);
                                break;
                            }
                        }
                        Macrostep::WrapUp | Macrostep::Quiesce | Macrostep::Disarm => {
                            report.max_macrosteps =
                                report.max_macrosteps.max(step.saturating_add(1));
                            terminated = true;
                            break;
                        }
                        Macrostep::NoOp => unreachable!(),
                    }
                }
                if !terminated {
                    report.unterminated_episodes = report.unterminated_episodes.saturating_add(1);
                }
            }
        }
    }
    report
}
