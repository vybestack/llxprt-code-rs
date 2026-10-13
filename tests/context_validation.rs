//! Phase 6 red specification: a landed operation must still require a gate receipt.
use llxprt_code_rs::context_txn::{
    bound_port::BoundPort,
    budget::{Budget, Margins},
    executor::{Epoch, Executor, ExecutorError},
    operation::Proposer,
};
use std::rc::Rc;

#[test]
fn phase6_note_lands_but_generic_commit_cannot_bypass_derivation_receipt() {
    let mut executor = Executor::new_generous(Epoch(1));
    executor.bind_port(Rc::new(BoundPort::new(Margins::V1, 0, 0)));
    executor.propose_as("note", 1, Proposer::M).unwrap();
    executor.snapshot().unwrap();
    executor.generate().expect("Phase 6 note must be landed");
    let bytes = 32;
    let bound = executor.port_bound(0, bytes).unwrap();
    executor
        .validate(bound, &Budget { b: 1 << 20, r: 8, h: 4 }, 0, 0, 0, bytes)
        .unwrap();
    let before = executor.chain_head();
    assert_eq!(
        executor.commit(1, None, bound),
        Err(ExecutorError::PreconditionFailed { which: "semantic-receipt-boundary" })
    );
    assert_eq!(executor.chain_head(), before);
}
