use super::tests::*;
use super::*;
use std::collections::{BTreeMap, BTreeSet};

#[test]
fn revalidation_does_not_replace_normative_authority_with_observation_authority() {
    let mut ledger = Ledger::default();
    let mut entry = obligation("decision");
    entry.freshness = Freshness::Stale;
    entry.read_set.insert("workspace".into(), 1);
    admit(&mut ledger, entry.clone());
    let observation = Observation {
        source: empirical("tool read").source,
        read_set: BTreeMap::from([("workspace".into(), 1)]),
        supports: BTreeSet::from([entry.id.clone()]),
    };
    ledger
        .apply(&Command::Revalidate { observation }, 2, &scopes())
        .unwrap();
    let refreshed = ledger.entry(&entry.id).unwrap();
    assert_eq!(refreshed.freshness, Freshness::Current);
    assert_eq!(refreshed.source, entry.source);
}

#[test]
fn normative_amendment_cannot_import_generated_authority_and_recomputes_conflicts() {
    let mut ledger = Ledger::default();
    let mut old = empirical("subject");
    old.kind = FactKind::Normative;
    old.source.authority = Authority::User;
    let mut other = old.clone();
    other.id.predicate = "other".into();
    other.fold.text = "other decision".into();
    admit(&mut ledger, old.clone());
    admit(&mut ledger, other.clone());
    let mut replacement = old.clone();
    replacement.id.predicate = "amended".into();
    replacement.fold.text = "amended decision".into();
    replacement.source.origin = Origin::Generated;
    let command = Command::Amend {
        fact: old.id.clone(),
        replacement: replacement.clone(),
        principal: old.source.clone(),
    };
    let before = ledger.clone();
    assert_eq!(
        ledger.apply(&command, 2, &scopes()),
        Err(LedgerError::Authority)
    );
    assert_eq!(ledger, before);
    replacement.source.origin = Origin::Ingress;
    ledger
        .apply(
            &Command::Amend {
                fact: old.id.clone(),
                replacement: replacement.clone(),
                principal: old.source,
            },
            2,
            &scopes(),
        )
        .unwrap();
    let (left, right) = if replacement.id < other.id {
        (replacement.id, other.id)
    } else {
        (other.id, replacement.id)
    };
    assert_eq!(
        ledger.conflicts(),
        &BTreeSet::from([Conflict::Normative { left, right }])
    );
}
