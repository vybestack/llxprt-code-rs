use super::{
    audit::*,
    bound_port::BoundPort,
    budget::{Budget, Margins},
    executor::*,
};
use crate::context_kernel::{events::*, legality::*, reducer::*};
use std::rc::Rc;

fn log() -> EventLog {
    let mut log = EventLog::new(crate::context_kernel::migration::V2);
    let mut sequencer = Sequencer::continuing(&log, 1, 1_000);
    for kind in [
        EventKind::OperationCommit {
            class: OperationClass::ScopeOpen,
            subject: 1,
            argument: 0,
        },
        EventKind::Append {
            source: AppendSource::User,
            sanitized: b"do not deploy".to_vec(),
            scope: 1,
            claims: vec![],
        },
        EventKind::OperationCommit {
            class: OperationClass::Place,
            subject: 0,
            argument: 2,
        },
    ] {
        log.append(sequencer.append(kind, log.store_version()))
            .unwrap();
    }
    log
}
#[test]
fn shadow_audits_every_prefix_and_detects_cached_state_mutations() {
    let log = log();
    let contract = RenderContract::generous(1);
    assert_eq!(audit_log(&log, &contract), Ok(3));
    let state = Reducer::new(IDLENESS_WINDOW).fold(&log).unwrap();
    assert_eq!(audit_tip(&log, &state, &contract), Ok(()));
    for axis in 0..7 {
        let mut mutated = state.clone();
        match axis {
            0 => mutated.version += 1,
            1 => mutated.state_hash ^= 1,
            2 => mutated.store_mode = 2,
            3 => mutated.filter_rule_version += 1,
            4 => mutated.quiesced = true,
            5 => mutated
                .pins
                .push(crate::context_kernel::ir::ItemId::append(1)),
            _ => mutated.vocabulary_version += 1,
        }
        assert_eq!(
            audit_tip(&log, &mutated, &contract),
            Err(AuditError::StateMismatch),
            "axis {axis}"
        );
    }
}
#[test]
fn shadow_detects_pairing_budget_and_bad_event_framing() {
    let log = log();
    let mut contract = RenderContract::generous(1);
    contract.declare("read_file", "missing", ToolRole::Call);
    assert!(matches!(
        audit_log(&log, &contract),
        Err(AuditError::Legality(Violation::Pairing { .. }))
    ));
    let mut small = RenderContract::generous(1);
    small.profile_budget_units = 0;
    assert!(matches!(
        audit_log(&log, &small),
        Err(AuditError::Legality(Violation::ProfileOverBudget { .. }))
    ));
    let mut broken = log.events()[0].clone();
    broken.epoch += 1;
    let mut bad = EventLog::new(log.store_version());
    assert!(bad.append(broken).is_err());
}
#[test]
fn live_commit_refuses_invalid_effect_without_publishing_partial_state() {
    let mut ex = Executor::new_generous(Epoch(1));
    ex.bind_port(Rc::new(BoundPort::new(Margins::V1, 0, 0)));
    ex.propose("scope-open", 1).unwrap();
    ex.snapshot().unwrap();
    ex.generate().unwrap();
    let bound = ex.port_bound(0, 0).unwrap();
    ex.validate(
        bound,
        &Budget {
            b: 1_000,
            r: 8,
            h: 4,
        },
        0,
        0,
        0,
        0,
    )
    .unwrap();
    let before = ex.governed_store_version();
    let error = ex
        .commit(
            1,
            Some(EventKind::OperationCommit {
                class: OperationClass::Place,
                subject: 999,
                argument: 2,
            }),
            bound,
        )
        .unwrap_err();
    assert!(matches!(error, ExecutorError::Audit(AuditError::Replay(_))));
    assert_eq!(ex.governed_store_version(), before);
    assert_eq!(ex.region_admitted(), 0);
    assert_eq!(ex.state(), TxnState::Aborted);
}

#[test]
fn shadow_respects_the_durable_chain_anchor_after_restart() {
    let original = log();
    let mut resumed = EventLog::resuming(
        original.store_version(),
        original.next_sequence(),
        original.head_checksum(),
    );
    let mut writer = Sequencer::continuing(&resumed, 2, 2_000);
    resumed
        .append(writer.append(
            EventKind::OperationCommit {
                class: OperationClass::ScopeOpen,
                subject: 2,
                argument: 0,
            },
            resumed.store_version(),
        ))
        .unwrap();
    let state = Reducer::new(IDLENESS_WINDOW).fold(&resumed).unwrap();
    assert_eq!(
        audit_tip(&resumed, &state, &RenderContract::generous(1)),
        Ok(())
    );
    assert_eq!(audit_log(&resumed, &RenderContract::generous(1)), Ok(1));
}
