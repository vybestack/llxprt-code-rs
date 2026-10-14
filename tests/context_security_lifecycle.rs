//! Owner-authenticated pin/escalation and physical-erasure receipts.
use llxprt_code_rs::{
    context_kernel::{
        events::*,
        ir::Region,
        ledger::*,
        reducer::{Reducer, IDLENESS_WINDOW},
        security::*,
    },
    context_store::{
        store::{ContextStore, StoreMode},
        vault::VaultKey,
    },
    context_txn::{
        bound_port::BoundPort,
        budget::{Budget, Margins},
        executor::*,
        operation::{self, Proposer},
        security::*,
    },
};
use std::rc::Rc;
fn owner() -> Source {
    Source {
        leaf: "operator-ingress".into(),
        authority: Authority::Operator,
        origin: Origin::Ingress,
        scope: 1,
    }
}
struct Ports {
    store: ContextStore,
    receipt: Option<Erasure>,
}
impl SecurityPorts for Ports {
    fn principal_authenticated(&self, source: &Source) -> bool {
        source == &owner()
    }
    fn escalation_authorized(&self, e: &Escalation) -> bool {
        e.envelope_ceiling == 16 && e.after <= 16
    }
    fn erasure_completed(&self, e: &Erasure) -> bool {
        self.receipt.as_ref() == Some(e) && self.store.erasure_matches(e)
    }
}
fn budget() -> Budget {
    Budget {
        b: 1 << 24,
        r: 8,
        h: 4,
    }
}
fn prepare(ex: &mut Executor, name: &'static str) {
    ex.propose_as(name, 1, operation::find(name).unwrap().proposer)
        .unwrap();
    ex.snapshot().unwrap();
    ex.generate().unwrap();
}
fn executor() -> (Executor, FencingClock) {
    let clock = FencingClock::new();
    let mut ex = Executor::new_generous(clock.acquire());
    ex.bind_port(Rc::new(BoundPort::new(Margins::V1, 1, 0)));
    prepare(&mut ex, "scope-open");
    let bound = ex.port_bound(0, 0).unwrap();
    ex.validate(bound, &budget(), 0, 0, 0, 0).unwrap();
    ex.commit(
        1,
        Some(EventKind::OperationCommit {
            class: OperationClass::ScopeOpen,
            subject: 1,
            argument: 0,
        }),
        bound,
    )
    .unwrap();
    prepare(&mut ex, "admit-ingress");
    let bound = ex.port_bound(0, 12).unwrap();
    ex.validate(bound, &budget(), 0, 0, 0, 12).unwrap();
    ex.commit(
        1,
        Some(EventKind::Append {
            source: AppendSource::ToolResult {
                call_id: "tool".into(),
                tool: "read_file".into(),
            },
            sanitized: b"placed claim".to_vec(),
            scope: 1,
            claims: vec![],
        }),
        bound,
    )
    .unwrap();
    prepare(&mut ex, "place");
    let bound = ex.port_bound(0, 0).unwrap();
    ex.validate(bound, &budget(), 0, 0, 0, 0).unwrap();
    ex.commit(
        1,
        Some(EventKind::OperationCommit {
            class: OperationClass::Place,
            subject: 0,
            argument: Region::Notes.rank(),
        }),
        bound,
    )
    .unwrap();
    (ex, clock)
}
fn commit(
    ex: &mut Executor,
    clock: &FencingClock,
    event: SecurityEvent,
    ports: &Ports,
) -> Result<(), SecurityCommitError> {
    prepare(ex, event.operation());
    let receipt = ex.validate_security(event, ports, &budget())?;
    ex.commit_security(1, clock, receipt, ports)?;
    Ok(())
}
fn ports() -> Ports {
    Ports {
        store: ContextStore::open(&VaultKey::from([2; 32])),
        receipt: None,
    }
}
#[test]
fn pin_abuse_caps_expiry_owner_and_fenced_replay() {
    let (mut ex, clock) = executor();
    let ports = ports();
    let pin = Pin {
        item: 0,
        owner: owner(),
        expires: 20,
    };
    let mut wrong = pin.clone();
    wrong.owner.authority = Authority::Environment;
    assert!(commit(&mut ex, &clock, SecurityEvent::Pin(wrong), &ports).is_err());
    let mut unbounded = pin.clone();
    unbounded.expires = u64::MAX;
    assert!(commit(&mut ex, &clock, SecurityEvent::Pin(unbounded), &ports).is_err());
    commit(&mut ex, &clock, SecurityEvent::Pin(pin.clone()), &ports).unwrap();
    assert_eq!(ex.security().pins().len(), 1);
    assert!(commit(&mut ex, &clock, SecurityEvent::Pin(pin), &ports).is_err());
    let mut forged = owner();
    forged.leaf = "attacker".into();
    assert!(commit(
        &mut ex,
        &clock,
        SecurityEvent::Unpin {
            item: 0,
            owner: forged
        },
        &ports
    )
    .is_err());
    commit(
        &mut ex,
        &clock,
        SecurityEvent::Unpin {
            item: 0,
            owner: owner(),
        },
        &ports,
    )
    .unwrap();
    assert!(ex.security().pins().is_empty());
    let replay = Reducer::new(IDLENESS_WINDOW)
        .fold(ex.governed_log())
        .unwrap();
    assert_eq!(&replay.security, ex.security());
    prepare(&mut ex, "pin");
    let bytes = 100;
    let bound = ex.port_bound(0, bytes).unwrap();
    ex.validate(bound, &budget(), 0, 0, 0, bytes).unwrap();
    assert!(ex
        .commit(
            1,
            Some(EventKind::OperationCommit {
                class: OperationClass::Pin,
                subject: 1,
                argument: 0
            }),
            bound
        )
        .is_err());
}
#[test]
fn escalation_is_pending_then_completed_only_inside_operator_envelope() {
    let (mut ex, clock) = executor();
    let ports = ports();
    let mut e = Escalation {
        id: "raise".into(),
        operator: owner(),
        before: 4,
        after: 8,
        envelope_ceiling: 16,
        complete: false,
    };
    let mut forged = e.clone();
    forged.envelope_ceiling = 1_000;
    forged.after = 999;
    assert!(commit(
        &mut ex,
        &clock,
        SecurityEvent::EscalatePending(forged),
        &ports
    )
    .is_err());
    e.operator.authority = Authority::User;
    assert!(commit(
        &mut ex,
        &clock,
        SecurityEvent::EscalatePending(e.clone()),
        &ports
    )
    .is_err());
    e.operator = owner();
    commit(&mut ex, &clock, SecurityEvent::EscalatePending(e), &ports).unwrap();
    assert!(!ex.security().escalations()["raise"].complete);
    commit(
        &mut ex,
        &clock,
        SecurityEvent::EscalateComplete {
            id: "raise".into(),
            operator: owner(),
        },
        &ports,
    )
    .unwrap();
    assert!(ex.security().escalations()["raise"].complete);
    assert!(commit(
        &mut ex,
        &clock,
        SecurityEvent::EscalateComplete {
            id: "raise".into(),
            operator: owner()
        },
        &ports
    )
    .is_err());
}
#[test]
fn erased_vault_and_dependent_derivative_are_unverified_after_replay() {
    let (mut ex, clock) = executor();
    let mut ports = ports();
    let handle = ports
        .store
        .vault_put(b"secret body", "security-test")
        .unwrap();
    let erasure = Erasure {
        leaf: "secret-leaf".into(),
        vault_handle: handle.clone(),
        post_checkpoint: hash(b"durable-post-erasure"),
        pre_hashes_unverifiable: true,
    };
    assert!(commit(
        &mut ex,
        &clock,
        SecurityEvent::Erase(erasure.clone()),
        &ports
    )
    .is_err());
    ports.store.set_mode(StoreMode::ReadOnly);
    assert!(ports.store.vault_erase(&handle).is_err());
    assert!(ports.store.vault_get(&handle).is_ok());
    ports.store.set_mode(StoreMode::Normal);
    let erasure = ports
        .store
        .erase_with_receipt("secret-leaf", &handle)
        .unwrap();
    assert!(ports.store.erasure_matches(&erasure));
    ports.receipt = Some(erasure.clone());
    commit(&mut ex, &clock, SecurityEvent::Erase(erasure), &ports).unwrap();
    assert!(ports.store.vault_get(&handle).is_err());
    let snapshot = ports.store.vault_snapshot();
    let mut recovered = ContextStore::open(&VaultKey::from([2; 32]));
    recovered.restore_vault(snapshot).unwrap();
    assert!(recovered.vault_get(&handle).is_err());
    let replay = Reducer::new(IDLENESS_WINDOW)
        .fold(ex.governed_log())
        .unwrap();
    assert_eq!(&replay.security, ex.security());
    assert!(ex.security().erased()["secret-leaf"].pre_hashes_unverifiable);
    assert_eq!(operation::find("redact").unwrap().proposer, Proposer::O);
}

#[test]
fn pin_expiry_and_stale_fence_fail_closed_without_losing_retained_state() {
    let (mut ex, clock) = executor();
    let ports = ports();
    let expires = ex.governed_log().events().last().unwrap().sequence + 12;
    commit(
        &mut ex,
        &clock,
        SecurityEvent::Pin(Pin {
            item: 0,
            owner: owner(),
            expires,
        }),
        &ports,
    )
    .unwrap();
    // Advance writer through replayable no-effect transactions, never rewrite time.
    for _ in 0..12 {
        prepare(&mut ex, "place");
        let bound = ex.port_bound(0, 0).unwrap();
        ex.validate(bound, &budget(), 0, 0, 0, 0).unwrap();
        ex.commit(
            1,
            Some(EventKind::OperationCommit {
                class: OperationClass::Place,
                subject: 0,
                argument: Region::Notes.rank(),
            }),
            bound,
        )
        .unwrap();
    }
    commit(
        &mut ex,
        &clock,
        SecurityEvent::Expire { sequence: expires },
        &ports,
    )
    .unwrap();
    assert!(ex.security().pins().is_empty());
    prepare(&mut ex, "pin");
    let receipt = ex
        .validate_security(
            SecurityEvent::Pin(Pin {
                item: 0,
                owner: owner(),
                expires: ex.governed_log().events().last().unwrap().sequence + 20,
            }),
            &ports,
            &budget(),
        )
        .unwrap();
    clock.acquire();
    assert!(ex.commit_security(1, &clock, receipt, &ports).is_err());
    assert!(ex.security().pins().is_empty());
}

#[test]
fn pin_cap_is_enforced_on_distinct_placed_items_with_no_partial_mutation() {
    let (mut ex, clock) = executor();
    let ports = ports();
    // Place enough admitted items, then mint exactly the fixed cap.
    for index in 0..PIN_CAP {
        prepare(&mut ex, "admit-ingress");
        let bytes = 12;
        let bound = ex.port_bound(0, bytes).unwrap();
        ex.validate(bound, &budget(), 0, 0, 0, bytes).unwrap();
        ex.commit(
            1,
            Some(EventKind::Append {
                source: AppendSource::ToolResult {
                    call_id: "pin-test".into(),
                    tool: "read_file".into(),
                },
                sanitized: b"placed claim".to_vec(),
                scope: 1,
                claims: vec![],
            }),
            bound,
        )
        .unwrap();
        let item = index as u64 + 1;
        prepare(&mut ex, "place");
        let bound = ex.port_bound(0, 0).unwrap();
        ex.validate(bound, &budget(), 0, 0, 0, 0).unwrap();
        ex.commit(
            1,
            Some(EventKind::OperationCommit {
                class: OperationClass::Place,
                subject: item,
                argument: Region::Notes.rank(),
            }),
            bound,
        )
        .unwrap();
    }
    for item in 0..PIN_CAP as u64 {
        let expires = ex.governed_log().events().last().unwrap().sequence + PIN_TTL;
        commit(
            &mut ex,
            &clock,
            SecurityEvent::Pin(Pin {
                item,
                owner: owner(),
                expires,
            }),
            &ports,
        )
        .unwrap();
    }
    assert_eq!(ex.security().pins().len(), PIN_CAP);
    let expires = ex.governed_log().events().last().unwrap().sequence + PIN_TTL;
    assert!(commit(
        &mut ex,
        &clock,
        SecurityEvent::Pin(Pin {
            item: PIN_CAP as u64,
            owner: owner(),
            expires
        }),
        &ports
    )
    .is_err());
    assert_eq!(ex.security().pins().len(), PIN_CAP);
}
