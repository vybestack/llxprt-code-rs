//! Progress macrostep: lexicographic decrease of `(Phi + mandatory_queue,
//! retries_remaining)` with no armed unquiesced no-op state.

/// Terminal outcomes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TerminalOutcome {
    Disarmed,
    WrapUp,
    Quiesced,
}

/// The macrostep measure: lexicographic `(Psi, retries_remaining)`.
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

/// A macrostep action.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Macrostep {
    NoOp,
    Reclaim,
    WrapUp,
    Quiesce,
    Disarm,
}

/// Lexicographic `(Psi, retries_remaining)` decrease test.
pub fn lexicographically_decreases(before: ProgressState, after: ProgressState) -> bool {
    after.psi < before.psi
        || (after.psi == before.psi && after.retries_remaining < before.retries_remaining)
}

/// Terminal reserve: WrapUp must stay feasible.
pub fn terminal_reserve(wrap_up_cost: u64, available: u64) -> bool {
    wrap_up_cost <= available
}

/// Choose the next action so that no armed state is an unquiesced no-op.
pub fn next_action(
    current: ProgressState,
    armed: bool,
    terminal: Option<TerminalOutcome>,
) -> Macrostep {
    match terminal {
        Some(TerminalOutcome::Disarmed) => Macrostep::Disarm,
        Some(TerminalOutcome::WrapUp) => Macrostep::WrapUp,
        Some(TerminalOutcome::Quiesced) => Macrostep::Quiesce,
        None if armed && current.retries_remaining > 0 => Macrostep::Reclaim,
        None if armed => Macrostep::Quiesce,
        None => Macrostep::Disarm,
    }
}
