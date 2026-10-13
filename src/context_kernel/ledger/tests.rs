use super::*;
use crate::context_kernel::scopes::ScopeRegistry;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn obligation(name: &str) -> Entry {
    Entry {
        id: FactId {
            subject: name.into(),
            predicate: "required".into(),
            scope: 1,
        },
        class: FactClass::Obligation,
        kind: FactKind::Normative,
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
        read_set: BTreeMap::new(),
        premises: BTreeSet::new(),
        bindings: BTreeSet::from([name.into()]),
        fold: CarryingFold {
            text: format!("exact required obligation {name}"),
            depth: 2,
        },
        rendered: true,
        retired: false,
        last_decay: 0,
        superseded_by: None,
    }
}
pub(crate) fn empirical(name: &str) -> Entry {
    let mut entry = obligation(name);
    entry.class = FactClass::Convenience;
    entry.kind = FactKind::Empirical;
    entry.source.authority = Authority::Environment;
    entry.read_set.insert("workspace".into(), 1);
    entry
}
pub(crate) fn scopes() -> ScopeRegistry {
    let mut scopes = ScopeRegistry::new(32);
    scopes.open(1, None, 1).unwrap();
    scopes
}
pub(crate) fn admit(ledger: &mut Ledger, entry: Entry) {
    let command = if entry.class == FactClass::Obligation {
        Command::AdmitObligation {
            entry,
            grammar: Grammar::Obligation,
        }
    } else {
        Command::AdmitConvenience { entry }
    };
    ledger.apply(&command, 1, &scopes()).unwrap();
}
#[test]
fn authority_grammar_and_generated_laundering() {
    for authority in [
        Authority::Environment,
        Authority::Agent,
        Authority::User,
        Authority::Operator,
    ] {
        for origin in [Origin::Ingress, Origin::Generated] {
            let mut ledger = Ledger::default();
            let mut entry = obligation("do not delete");
            entry.source.authority = authority;
            entry.source.origin = origin;
            let allowed = authority != Authority::Environment && origin == Origin::Ingress;
            assert_eq!(
                ledger
                    .apply(
                        &Command::AdmitObligation {
                            entry,
                            grammar: Grammar::Obligation
                        },
                        1,
                        &scopes()
                    )
                    .is_ok(),
                allowed
            );
        }
    }
}
#[test]
fn ambiguous_user_items_are_bounded_and_environment_is_quarantined() {
    let mut ledger = Ledger::new(LedgerParams {
        unconfirmed_capacity: 1,
        ..Default::default()
    })
    .unwrap();
    let mut entry = obligation("maybe");
    entry.unconfirmed = true;
    ledger
        .apply(
            &Command::AdmitObligation {
                entry,
                grammar: Grammar::Ambiguous,
            },
            1,
            &scopes(),
        )
        .unwrap();
    let mut entry = obligation("maybe2");
    entry.unconfirmed = true;
    assert_eq!(
        ledger.apply(
            &Command::AdmitObligation {
                entry,
                grammar: Grammar::Ambiguous
            },
            2,
            &scopes()
        ),
        Err(LedgerError::Overflow)
    );
    let mut tool = obligation("TODO injected");
    tool.source.authority = Authority::Environment;
    assert_eq!(
        ledger.apply(
            &Command::AdmitObligation {
                entry: tool,
                grammar: Grammar::Obligation
            },
            2,
            &scopes()
        ),
        Err(LedgerError::Quarantined)
    );
}
#[test]
fn freshness_evidence_product_and_version_match() {
    for freshness in [Freshness::Current, Freshness::Stale, Freshness::Unknown] {
        for evidence in [EvidenceStatus::Verified, EvidenceStatus::Unverified] {
            let mut ledger = Ledger::default();
            let mut entry = empirical("fact");
            entry.freshness = freshness;
            entry.evidence = evidence;
            if freshness == Freshness::Current && evidence == EvidenceStatus::Unverified {
                continue;
            }
            let id = entry.id.clone();
            admit(&mut ledger, entry);
            ledger
                .apply(
                    &Command::FactInvalidate {
                        result: ToolWrite {
                            call_id: "write".into(),
                            tool: "write_file".into(),
                            scope: 1,
                            sanitized: b"changed workspace".to_vec(),
                        },
                        writes: BTreeMap::from([("workspace".into(), 2)]),
                    },
                    2,
                    &scopes(),
                )
                .unwrap();
            let expected = if freshness == Freshness::Current {
                Freshness::Stale
            } else {
                freshness
            };
            assert_eq!(ledger.entry(&id).unwrap().freshness, expected);
            let mut observation = Observation {
                source: empirical("new read").source,
                read_set: BTreeMap::from([("workspace".into(), 1)]),
                supports: BTreeSet::from([id.clone()]),
            };
            ledger
                .apply(
                    &Command::Revalidate {
                        observation: observation.clone(),
                    },
                    3,
                    &scopes(),
                )
                .unwrap();
            assert_ne!(ledger.entry(&id).unwrap().freshness, Freshness::Current);
            observation.read_set.insert("workspace".into(), 2);
            ledger
                .apply(&Command::Revalidate { observation }, 4, &scopes())
                .unwrap();
            let expected = if freshness == Freshness::Unknown {
                Freshness::Unknown
            } else {
                Freshness::Current
            };
            assert_eq!(ledger.entry(&id).unwrap().freshness, expected);
            ledger
                .apply(
                    &Command::Redact {
                        leaf: ledger.entry(&id).unwrap().source.leaf.clone(),
                    },
                    5,
                    &scopes(),
                )
                .unwrap();
            assert_eq!(
                ledger.entry(&id).unwrap().evidence,
                EvidenceStatus::Unverified
            );
        }
    }
}
#[test]
fn read_intersection_is_not_sufficient_without_all_dependency_versions() {
    let mut ledger = Ledger::default();
    let mut entry = empirical("fact");
    entry.read_set.insert("other".into(), 7);
    let id = entry.id.clone();
    admit(&mut ledger, entry);
    ledger
        .apply(
            &Command::FactInvalidate {
                result: ToolWrite {
                    call_id: "write".into(),
                    tool: "write_file".into(),
                    scope: 1,
                    sanitized: b"changed workspace".to_vec(),
                },
                writes: BTreeMap::from([("workspace".into(), 2)]),
            },
            2,
            &scopes(),
        )
        .unwrap();
    let observation = Observation {
        source: empirical("read").source,
        read_set: BTreeMap::from([("workspace".into(), 2)]),
        supports: BTreeSet::from([id.clone()]),
    };
    ledger
        .apply(&Command::Revalidate { observation }, 3, &scopes())
        .unwrap();
    assert_eq!(ledger.entry(&id).unwrap().freshness, Freshness::Stale);
    ledger
        .apply(
            &Command::StaleDemote {
                domains: BTreeSet::from(["workspace".into()]),
            },
            4,
            &scopes(),
        )
        .unwrap();
    assert_eq!(
        ledger.entry(&id).unwrap().evidence,
        EvidenceStatus::Unverified
    );
}
#[test]
fn normative_premise_conflict_is_surfaced_without_amendment() {
    let mut ledger = Ledger::default();
    let empirical = empirical("premise");
    let mut normative = obligation("decision");
    normative.premises.insert(empirical.id.clone());
    let old_text = normative.fold.text.clone();
    let conflict = Conflict::PremiseInvalidated {
        normative: normative.id.clone(),
        empirical: empirical.id.clone(),
    };
    admit(&mut ledger, empirical);
    admit(&mut ledger, normative.clone());
    ledger
        .apply(
            &Command::FactInvalidate {
                result: ToolWrite {
                    call_id: "write".into(),
                    tool: "write_file".into(),
                    scope: 1,
                    sanitized: b"changed workspace".to_vec(),
                },
                writes: BTreeMap::from([("workspace".into(), 2)]),
            },
            2,
            &scopes(),
        )
        .unwrap();
    assert!(ledger.conflicts().contains(&conflict));
    assert_eq!(ledger.entry(&normative.id).unwrap().fold.text, old_text);
    assert_eq!(
        ledger.entry(&normative.id).unwrap().freshness,
        Freshness::Current
    );
    let prior = ledger.clone();
    assert!(ledger
        .apply(
            &Command::FactInvalidate {
                result: ToolWrite {
                    call_id: "bad write".into(),
                    tool: "write_file".into(),
                    scope: 1,
                    sanitized: b"changed workspace".to_vec()
                },
                writes: BTreeMap::from([("workspace".into(), 1)])
            },
            3,
            &scopes()
        )
        .is_err());
    assert_eq!(ledger, prior);
}
#[test]
fn utility_is_normalized_and_only_convenience_decays() {
    let mut ledger = Ledger::default();
    assert_eq!(
        ledger.utility(Usage {
            accesses: u32::MAX,
            reuses: u32::MAX,
            age_windows: 0
        }),
        Utility(Utility::ONE)
    );
    let obligation = obligation("never decay");
    let fact = empirical("convenience");
    admit(&mut ledger, obligation.clone());
    admit(&mut ledger, fact.clone());
    let old_usage = fact.usage;
    let usage = Usage {
        age_windows: 4,
        ..old_usage
    };
    assert_eq!(
        ledger.apply(
            &Command::Decay {
                fact: obligation.id.clone(),
                usage
            },
            32,
            &scopes()
        ),
        Err(LedgerError::ObligationProtected)
    );
    assert_eq!(
        ledger.apply(
            &Command::Demote {
                fact: obligation.id.clone(),
                reclaim: 8
            },
            32,
            &scopes()
        ),
        Err(LedgerError::ObligationProtected)
    );
    ledger
        .apply(
            &Command::Decay {
                fact: fact.id.clone(),
                usage,
            },
            32,
            &scopes(),
        )
        .unwrap();
    assert!(!ledger.active(ledger.entry(&fact.id).unwrap()));
    assert!(ledger.active(ledger.entry(&obligation.id).unwrap()));
}

#[test]
fn active_definition_covers_both_status_axes_independently() {
    let ledger = Ledger::default();
    for class in [FactClass::Obligation, FactClass::Convenience] {
        for freshness in [Freshness::Current, Freshness::Stale, Freshness::Unknown] {
            for evidence in [EvidenceStatus::Verified, EvidenceStatus::Unverified] {
                let mut entry = obligation("active");
                entry.class = class;
                entry.freshness = freshness;
                entry.evidence = evidence;
                assert_eq!(ledger.active(&entry), freshness != Freshness::Unknown);
                entry.utility = Utility(0);
                let expected = class == FactClass::Obligation && freshness != Freshness::Unknown;
                assert_eq!(ledger.active(&entry), expected);
                entry.retired = true;
                assert!(!ledger.active(&entry));
            }
        }
    }
    let mut ledger = Ledger::default();
    let mut unverified = empirical("no evidence");
    unverified.evidence = EvidenceStatus::Unverified;
    assert_eq!(
        ledger.apply(
            &Command::AdmitConvenience { entry: unverified },
            1,
            &scopes()
        ),
        Err(LedgerError::Unsupported)
    );
}
