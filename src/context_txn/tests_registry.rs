//! Registry-adjacent unit tests split from `tests.rs` to keep that file
//! inside the xtask quality effective-LOC ceiling.

use super::executor::{Epoch, Executor, ExecutorError};
use super::operation::Proposer;

/// An executor on the default lease epoch with an agreeing port, mirroring
/// the `armed_executor` fixture in `tests.rs`.
fn armed_executor(governed: u64) -> Executor {
    let mut ex = Executor::new_generous(Epoch(1));
    ex.bind_port(std::rc::Rc::new(super::bound_port::BoundPort::new(
        super::budget::Margins::V1,
        0,
        governed,
    )));
    ex
}

/// unknown rows are rejected at propose.
#[test]
fn unknown_rows_are_rejected() {
    let mut ex = armed_executor(0);
    assert_eq!(
        ex.propose("no-such-op", 1),
        Err(ExecutorError::CapabilityNotLanded { op: "no-such-op" })
    );
}

/// GREEN: proposer letters round-trip.
#[test]
fn proposer_letters_round_trip() {
    assert_eq!(Proposer::S.as_str(), "S");
    assert_eq!(Proposer::C.as_str(), "C");
    assert_eq!(Proposer::M.as_str(), "M");
    assert_eq!(Proposer::O.as_str(), "O");
    assert_eq!(Proposer::L.as_str(), "L");
}
