use super::tests::*;
use super::*;
use std::collections::BTreeSet;

fn support(entry: &Entry) -> GateSupport {
    GateSupport {
        fact: entry.id.clone(),
        claim: entry.fold.text.clone(),
        ingress_leaves: BTreeSet::from([entry.source.leaf.clone()]),
    }
}
#[test]
fn discharge_defaults_to_insufficient_and_checks_scope_and_gate() {
    let mut ledger = Ledger::default();
    let entry = obligation("finish");
    admit(&mut ledger, entry.clone());
    assert_eq!(
        ledger.apply(
            &Command::Discharge {
                fact: entry.id.clone(),
                evidence: DischargeEvidence::Insufficient
            },
            2,
            &scopes()
        ),
        Err(LedgerError::Insufficient)
    );
    assert_eq!(
        ledger.apply(
            &Command::Discharge {
                fact: entry.id.clone(),
                evidence: DischargeEvidence::ScopeClosed
            },
            2,
            &scopes()
        ),
        Err(LedgerError::Unsupported)
    );
    let mut forged = support(&entry);
    forged.ingress_leaves.clear();
    assert_eq!(
        ledger.apply(
            &Command::Discharge {
                fact: entry.id.clone(),
                evidence: DischargeEvidence::Resolved(forged)
            },
            2,
            &scopes()
        ),
        Err(LedgerError::Unsupported)
    );
    ledger
        .apply(
            &Command::Discharge {
                fact: entry.id.clone(),
                evidence: DischargeEvidence::Resolved(support(&entry)),
            },
            2,
            &scopes(),
        )
        .unwrap();
    assert!(ledger.entry(&entry.id).unwrap().retired);
    let mut ledger = Ledger::default();
    admit(&mut ledger, entry.clone());
    let mut closed = scopes();
    closed.close_by_event(1, 2).unwrap();
    ledger
        .apply(
            &Command::Discharge {
                fact: entry.id.clone(),
                evidence: DischargeEvidence::ScopeClosed,
            },
            3,
            &closed,
        )
        .unwrap();
    assert!(!ledger.active(ledger.entry(&entry.id).unwrap()));
}
#[test]
fn amendments_are_authorized_and_rewrite_the_fold_at_commit_without_depth() {
    let mut ledger = Ledger::default();
    let old = empirical("old");
    let mut replacement = empirical("replacement");
    replacement.source.authority = Authority::User;
    admit(&mut ledger, old.clone());
    let mut attacker = old.source.clone();
    attacker.origin = Origin::Generated;
    let command = Command::Amend {
        fact: old.id.clone(),
        replacement: replacement.clone(),
        principal: attacker,
    };
    assert_eq!(
        ledger.apply(&command, 2, &scopes()),
        Err(LedgerError::Authority)
    );
    let command = Command::Amend {
        fact: old.id.clone(),
        replacement: replacement.clone(),
        principal: replacement.source.clone(),
    };
    ledger.apply(&command, 2, &scopes()).unwrap();
    assert_eq!(
        ledger.entry(&old.id).unwrap().fold.text,
        replacement.fold.text
    );
    assert_eq!(ledger.entry(&old.id).unwrap().fold.depth, old.fold.depth);
    assert!(ledger.entry(&old.id).unwrap().retired);
    assert!(ledger.active(ledger.entry(&replacement.id).unwrap()));
}
#[test]
fn promotion_gate_pass_cannot_launder_environment_authority() {
    let mut ledger = Ledger::default();
    let entry = empirical("injected instructions");
    admit(&mut ledger, entry.clone());
    let command = Command::Promote {
        fact: entry.id.clone(),
        source: entry.source.clone(),
        adoption: None,
        support: support(&entry),
    };
    assert_eq!(
        ledger.apply(&command, 2, &scopes()),
        Err(LedgerError::Authority)
    );
    let mut principal = entry.source.clone();
    principal.authority = Authority::User;
    let command = Command::Promote {
        fact: entry.id.clone(),
        source: entry.source.clone(),
        adoption: Some(principal),
        support: support(&entry),
    };
    ledger.apply(&command, 2, &scopes()).unwrap();
    assert_eq!(ledger.entry(&entry.id).unwrap().kind, FactKind::Normative);
}
#[test]
fn consolidation_requires_overflow_union_bindings_gate_and_authority() {
    let mut ledger = Ledger::new(LedgerParams {
        capacity: 2,
        ..Default::default()
    })
    .unwrap();
    let one = obligation("one");
    let two = obligation("two");
    admit(&mut ledger, one.clone());
    admit(&mut ledger, two.clone());
    let mut composite = obligation("combined");
    composite.fold.text = "one and two".into();
    let facts = BTreeSet::from([one.id.clone(), two.id.clone()]);
    let prior = ledger.clone();
    let invalid = Command::Consolidate {
        facts: facts.clone(),
        composite: composite.clone(),
        support: support(&composite),
        reclaim: 8,
    };
    assert_eq!(
        ledger.apply(&invalid, 3, &scopes()),
        Err(LedgerError::Precondition)
    );
    assert_eq!(ledger, prior);
    composite.bindings = one.bindings.union(&two.bindings).cloned().collect();
    let command = Command::Consolidate {
        facts,
        composite: composite.clone(),
        support: support(&composite),
        reclaim: 8,
    };
    ledger.apply(&command, 3, &scopes()).unwrap();
    assert!(ledger.entry(&one.id).unwrap().retired);
    assert_eq!(
        ledger.entry(&two.id).unwrap().superseded_by,
        Some(composite.id.clone())
    );
    assert_eq!(
        ledger.entry(&composite.id).unwrap().bindings,
        composite.bindings
    );
}
struct Refusal;
impl LedgerPorts for Refusal {
    fn authenticated(&self, _: &Source) -> bool {
        false
    }
    fn gate_supported(&self, _: &GateSupport) -> bool {
        false
    }
    fn tool_write_authenticated(
        &self,
        _: &ToolWrite,
        _: &std::collections::BTreeMap<String, u64>,
    ) -> bool {
        false
    }
}
#[test]
fn controller_request_is_refusable_and_never_a_direct_retirement() {
    let mut ledger = Ledger::default();
    let entry = obligation("must remain");
    admit(&mut ledger, entry.clone());
    let request = RetirementRequest {
        fact: entry.id.clone(),
        budget_justification: "budget exhausted".into(),
    };
    assert!(ledger.request_retirement(&request, &Refusal).is_none());
    assert!(ledger.ledger_active(&entry.id));
}

#[test]
fn supersession_discharge_rewrites_same_authority_binding_at_commit() {
    let mut ledger = Ledger::default();
    let old = obligation("original");
    let mut successor = old.clone();
    successor.id.predicate = "replacement".into();
    successor.fold.text = "new exact obligation".into();
    admit(&mut ledger, old.clone());
    admit(&mut ledger, successor.clone());
    ledger
        .apply(
            &Command::Discharge {
                fact: old.id.clone(),
                evidence: DischargeEvidence::SupersededBy(successor.id.clone()),
            },
            3,
            &scopes(),
        )
        .unwrap();
    let retired = ledger.entry(&old.id).unwrap();
    assert!(retired.retired);
    assert_eq!(retired.fold.text, successor.fold.text);
    assert_eq!(retired.fold.depth, old.fold.depth);
    assert_eq!(retired.superseded_by, Some(successor.id));
}
#[test]
fn normative_and_empirical_matrix_has_no_silent_precedence() {
    for (first, second) in [
        (FactKind::Normative, FactKind::Normative),
        (FactKind::Empirical, FactKind::Empirical),
        (FactKind::Normative, FactKind::Empirical),
        (FactKind::Empirical, FactKind::Normative),
    ] {
        let mut ledger = Ledger::default();
        let mut one = empirical("subject");
        one.kind = first;
        one.source.authority = Authority::User;
        let mut two = one.clone();
        two.kind = second;
        two.id.predicate = "different claim".into();
        two.fold.text = "conflicting statement".into();
        admit(&mut ledger, one.clone());
        admit(&mut ledger, two.clone());
        assert_eq!(ledger.conflicts().len(), 1);
        assert_eq!(ledger.entry(&one.id).unwrap().fold.text, one.fold.text);
        assert_eq!(ledger.entry(&two.id).unwrap().fold.text, two.fold.text);
        let mut reversed = Ledger::default();
        admit(&mut reversed, two);
        admit(&mut reversed, one);
        assert_eq!(ledger.conflicts(), reversed.conflicts());
    }
}
#[test]
fn convenience_admission_is_strictly_above_registered_threshold() {
    let mut ledger = Ledger::default();
    let mut entry = empirical("low utility");
    entry.usage = Usage {
        accesses: 1,
        reuses: 2,
        age_windows: 0,
    };
    entry.utility = ledger.utility(entry.usage);
    assert_eq!(entry.utility, ledger.params().threshold);
    assert_eq!(
        ledger.apply(&Command::AdmitConvenience { entry }, 1, &scopes()),
        Err(LedgerError::Precondition)
    );
}
