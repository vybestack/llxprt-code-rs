//! Seeded *reachable* event sequences, rather than an independent Cartesian
//! product of scalar flags. This component screen does not claim coverage of
//! provider outcomes, every policy-mode product, or 48-hour execution.
use super::audit::{audit_log, audit_tip};
use crate::context_kernel::{
    events::*,
    ledger::{Authority, Origin, Source},
    legality::RenderContract,
    reducer::{Reducer, IDLENESS_WINDOW},
    security::{Pin, SecurityEvent},
};

fn append(log: &mut EventLog, kind: EventKind) {
    let mut writer = Sequencer::continuing(log, 1, 1_000);
    log.append(writer.append(kind, log.store_version()))
        .unwrap();
}
fn operation(log: &mut EventLog, class: OperationClass, subject: u64, argument: u64) {
    append(
        log,
        EventKind::OperationCommit {
            class,
            subject,
            argument,
        },
    );
}
#[test]
fn reachable_sequences_replay_after_every_prefix_and_chain_boundary() {
    for seed in 1u64..=64 {
        let mut random = seed;
        let mut log = EventLog::new(crate::context_kernel::migration::V2);
        for scope in 1..=8 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            operation(&mut log, OperationClass::ScopeOpen, scope, 0);
            let id = scope - 1;
            let owner = Source {
                leaf: format!("user-{scope}"),
                authority: Authority::User,
                origin: Origin::Ingress,
                scope,
            };
            append(
                &mut log,
                EventKind::Append {
                    source: AppendSource::User,
                    sanitized: format!("seed {seed} / scope {scope}: keep the constraint")
                        .into_bytes(),
                    scope,
                    claims: vec![],
                },
            );
            operation(&mut log, OperationClass::Place, scope - 1, 2);
            if random & 1 != 0 {
                let expiry = log.next_sequence() + 10;
                append(
                    &mut log,
                    EventKind::Security {
                        event: Box::new(SecurityEvent::Pin(Pin {
                            item: id,
                            owner: owner.clone(),
                            expires: expiry,
                        })),
                    },
                );
                append(
                    &mut log,
                    EventKind::Security {
                        event: Box::new(SecurityEvent::Unpin { item: id, owner }),
                    },
                );
            }
            operation(&mut log, OperationClass::ScopeCloseByEvent, scope, 0);
            let mut restored = EventLog::new(log.store_version());
            for event in log.events() {
                restored.append(event.clone()).unwrap();
            }
            let contract = RenderContract::generous(1);
            assert_eq!(
                audit_log(&restored, &contract),
                Ok(log.len()),
                "seed={seed} scope={scope}"
            );
            let state = Reducer::new(IDLENESS_WINDOW).fold(&restored).unwrap();
            assert_eq!(audit_tip(&restored, &state, &contract), Ok(()));
            assert_eq!(state, Reducer::new(IDLENESS_WINDOW).fold(&log).unwrap());
            // Framing corruption is refused before a modified event is published.
            let mut tampered = log.events()[0].clone();
            tampered.epoch = 999;
            let mut recipient = EventLog::new(log.store_version());
            assert!(recipient.append(tampered).is_err());
            assert!(recipient.is_empty());
        }
        operation(&mut log, OperationClass::QuiesceUnwritable, 0, 0);
        assert_eq!(audit_log(&log, &RenderContract::generous(1)), Ok(log.len()));
    }
}
