//! Phase 5 ledger state survives exact event replay, not an ignored marker.
use llxprt_code_rs::context_kernel::{
    events::{EventKind, EventLog, OperationClass, Sequencer},
    ledger::*,
    reducer::{Reducer, IDLENESS_WINDOW},
};
use std::collections::{BTreeMap, BTreeSet};
fn entry(name: &str, class: FactClass) -> Entry {
    Entry {
        id: FactId {
            subject: name.into(),
            predicate: "required".into(),
            scope: 1,
        },
        class,
        kind: if class == FactClass::Obligation {
            FactKind::Normative
        } else {
            FactKind::Empirical
        },
        source: Source {
            leaf: format!("ingress:{name}"),
            authority: Authority::User,
            origin: Origin::Ingress,
            scope: 1,
        },
        freshness: Freshness::Current,
        evidence: EvidenceStatus::Verified,
        unconfirmed: false,
        utility: Utility(6000),
        usage: Usage {
            accesses: 2,
            reuses: 2,
            age_windows: 0,
        },
        read_set: if class == FactClass::Obligation {
            BTreeMap::new()
        } else {
            BTreeMap::from([("workspace".into(), 1)])
        },
        premises: BTreeSet::new(),
        bindings: BTreeSet::from([name.into()]),
        fold: CarryingFold {
            text: name.into(),
            depth: 2,
        },
        rendered: true,
        retired: false,
        last_decay: 0,
        superseded_by: None,
    }
}
fn append(log: &mut EventLog, kind: EventKind) {
    let mut writer = Sequencer::continuing(log, 1, 0);
    log.append(writer.append(kind, 2)).unwrap();
}
#[test]
fn fact_conflict_restart_ledger_is_governed_state() {
    let mut log = EventLog::new(2);
    append(
        &mut log,
        EventKind::OperationCommit {
            class: OperationClass::ScopeOpen,
            subject: 1,
            argument: 0,
        },
    );
    let reducer = Reducer::new(IDLENESS_WINDOW);
    let before = reducer.fold(&log).unwrap();
    let empirical = entry("CTXEVAL-FACT-C5", FactClass::Convenience);
    let mut obligation = entry("CTXEVAL-OBLIGATION-O3", FactClass::Obligation);
    obligation.premises.insert(empirical.id.clone());
    append(
        &mut log,
        EventKind::Ledger {
            command: Box::new(Command::AdmitObligation {
                entry: obligation.clone(),
                grammar: Grammar::Obligation,
            }),
        },
    );
    append(
        &mut log,
        EventKind::Ledger {
            command: Box::new(Command::AdmitConvenience {
                entry: empirical.clone(),
            }),
        },
    );
    let admitted = reducer.fold(&log).unwrap();
    assert_ne!(
        before.state_hash, admitted.state_hash,
        "ledger cannot be an ignored event"
    );
    append(
        &mut log,
        EventKind::Ledger {
            command: Box::new(Command::FactInvalidate {
                result: ToolWrite {
                    call_id: "write-workspace".into(),
                    tool: "write_file".into(),
                    scope: 1,
                    sanitized: b"changed workspace".to_vec(),
                },
                writes: BTreeMap::from([("workspace".into(), 2)]),
            }),
        },
    );
    let after = reducer.fold(&log).unwrap();
    assert_eq!(
        after.ledger.entry(&obligation.id).unwrap().fold.text,
        "CTXEVAL-OBLIGATION-O3"
    );
    assert_eq!(
        after.ledger.entry(&empirical.id).unwrap().freshness,
        Freshness::Stale
    );
    assert_eq!(after.ledger.conflicts().len(), 1);
    let mut resumed = admitted.clone();
    reducer.fold_from(&mut resumed, &log).unwrap();
    assert_eq!(after, resumed, "restart from prefix matches cold replay");
    reducer.fold_from(&mut resumed, &log).unwrap();
    assert_eq!(after, resumed, "deduplication is deterministic");
}

#[test]
fn current_event_schema_refuses_placeholder_records() {
    let mut log = EventLog::new(2);
    let mut writer = Sequencer::continuing(&log, 1, 0);
    let mut record = writer.append(
        EventKind::Ledger {
            command: Box::new(Command::AdmitObligation {
                entry: entry("current format", FactClass::Obligation),
                grammar: Grammar::Obligation,
            }),
        },
        2,
    );
    record.schema_version = 1;
    assert!(matches!(
        log.append(record),
        Err(llxprt_code_rs::context_kernel::events::LogError::SchemaVersion { found: 1, .. })
    ));
    assert!(log.is_empty());
}
