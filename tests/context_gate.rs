//! Independent ingress-rooted oracle fixtures, including five adversarial generations.
use llxprt_code_rs::context_kernel::{
    lanes::{Lane, LanePolicyRegistry},
    ledger::{Authority, Ledger, Origin, Source},
    validation::*,
};
use std::collections::{BTreeMap, BTreeSet};

fn independence() -> Independence {
    Independence {
        compactor: "compact-a".into(),
        scorer: "score-b".into(),
    }
}
struct Store(BTreeMap<String, Leaf>);
impl LeafStore for Store {
    fn authenticated_leaf(&self, id: &str) -> Option<Leaf> {
        self.0.get(id).cloned()
    }
}
fn store() -> Store {
    Store(BTreeMap::from([
        (
            "leaf-a".into(),
            leaf("leaf-a", "Do not run deploy before tests in scope 1."),
        ),
        ("leaf-b".into(), leaf("leaf-b", "tests pass")),
    ]))
}
fn leaf(id: &str, text: &str) -> Leaf {
    Leaf {
        id: id.into(),
        sanitized: text.into(),
        source: Source {
            leaf: id.into(),
            origin: Origin::Ingress,
            authority: Authority::User,
            scope: 1,
        },
    }
}
fn fact() -> BoundFact {
    BoundFact {
        statement: "Do not run deploy before tests in scope 1.".into(),
        subject: "deploy".into(),
        predicate: "allowed".into(),
        scope: 1,
        command: "deploy".into(),
        negated: true,
        temporal: "before tests".into(),
    }
}
fn candidate() -> Candidate {
    Candidate {
        artifact: fact().statement.clone(),
        rendered_text: fact().statement.clone(),
        rendered: vec![fact()],
        roots: vec![
            Pointer::Ingress("leaf-a".into()),
            Pointer::Ingress("leaf-b".into()),
        ],
        graph: BTreeMap::new(),
        pending_notes: BTreeSet::new(),
        scope: 1,
    }
}
#[derive(Default)]
struct Consumers {
    required_down: bool,
    assertions_down: bool,
    score_down: bool,
    false_accept: bool,
    observed: Vec<Vec<String>>,
    contradiction: bool,
}
impl GateConsumers for Consumers {
    fn independence(&self) -> Independence {
        independence()
    }
    fn required_facts(&mut self, _: &[BoundFact]) -> Result<Vec<BoundFact>, GateError> {
        if self.required_down {
            Err(GateError::Consumer("required"))
        } else {
            Ok(Vec::new())
        }
    }
    fn assertions(&mut self, artifact: &str) -> Result<Vec<String>, GateError> {
        if self.assertions_down {
            Err(GateError::Consumer("assertions"))
        } else {
            Ok(artifact.lines().map(str::to_owned).collect())
        }
    }
    fn score(
        &mut self,
        assertion: &str,
        leaves: &[Leaf],
        _: &[BoundFact],
    ) -> Result<Score, GateError> {
        if self.score_down {
            return Err(GateError::Consumer("score"));
        }
        self.observed
            .push(leaves.iter().map(|l| l.id.clone()).collect());
        let supported = self.false_accept || leaves.iter().any(|l| l.sanitized.contains(assertion));
        Ok(Score {
            entailment: if supported { 10_000 } else { 0 },
            contradiction: self.contradiction,
        })
    }
}
fn validate(c: &Candidate, consumers: &mut Consumers) -> Result<ValidationRecord, GateError> {
    Gate::new(1, independence())
        .unwrap()
        .validate(c, &[fact()], &[fact()], &store(), consumers)
}
#[test]
fn closed_verdict_classes_and_distinct_escalation_responses() {
    let good = validate(&candidate(), &mut Consumers::default()).unwrap();
    assert_eq!(good.verdict, Verdict::Pass);
    assert_eq!(good.verdict.response(), Response::Commit);
    let mut c = candidate();
    c.rendered.clear();
    assert_eq!(
        validate(&c, &mut Consumers::default()).unwrap().verdict,
        Verdict::FailCompleteness {
            missing: vec![fact()]
        }
    );
    c = candidate();
    c.artifact.push_str("\nproduction is deployed");
    let verdict = validate(&c, &mut Consumers::default()).unwrap().verdict;
    assert_eq!(
        verdict,
        Verdict::FailPrecision {
            unsupported: vec!["production is deployed".into()]
        }
    );
    assert_eq!(verdict.response(), Response::RegenerateFromIngress);
    c.roots = vec![Pointer::Generated("note".into())];
    assert_eq!(
        validate(&c, &mut Consumers::default()).unwrap().verdict,
        Verdict::FailProvenance(ProvenanceFailure::UngroundedRoot)
    );
}
#[test]
fn tuple_binding_checks_do_not_accept_scattered_tokens() {
    for change in 0..7 {
        let mut c = candidate();
        let f = &mut c.rendered[0];
        match change {
            0 => f.command = "test".into(),
            1 => f.negated = false,
            2 => f.scope = 2,
            3 => f.temporal = "after tests".into(),
            4 => f.subject = "other".into(),
            5 => f.predicate = "denied".into(),
            _ => f.statement = "run deploy".into(),
        }
        c.artifact = format!(
            "{} deploy test before after tests 1 2 not",
            fact().statement
        );
        assert!(matches!(
            validate(&c, &mut Consumers::default()).unwrap().verdict,
            Verdict::FailCompleteness { .. }
        ));
    }
    let mut c = candidate();
    c.pending_notes.insert("unflushed-note".into());
    assert!(matches!(
        validate(&c, &mut Consumers::default()).unwrap().verdict,
        Verdict::FailCompleteness { .. }
    ));
}
#[test]
fn cycle_generated_note_and_forged_ingress_are_not_support() {
    let mut c = candidate();
    c.roots = vec![Pointer::Generated("a".into())];
    c.graph
        .insert("a".into(), vec![Pointer::Generated("b".into())]);
    c.graph
        .insert("b".into(), vec![Pointer::Generated("a".into())]);
    assert_eq!(
        validate(&c, &mut Consumers::default()).unwrap().verdict,
        Verdict::FailProvenance(ProvenanceFailure::Cycle)
    );
    c = candidate();
    let mut s = store();
    s.0.get_mut("leaf-a").unwrap().source.origin = Origin::Generated;
    let verdict = Gate::new(1, independence())
        .unwrap()
        .validate(&c, &[fact()], &[], &s, &mut Consumers::default())
        .unwrap()
        .verdict;
    assert_eq!(
        verdict,
        Verdict::FailProvenance(ProvenanceFailure::UngroundedRoot)
    );
    c.roots = vec![Pointer::Generated("note".into())];
    c.graph
        .insert("note".into(), vec![Pointer::Ingress("leaf-a".into())]);
    c.artifact = "production is deployed".into();
    assert!(matches!(
        validate(&c, &mut Consumers::default()).unwrap().verdict,
        Verdict::FailPrecision { .. }
    ));
}
#[test]
fn gate_recurrent_rooting_five_rounds_measure_false_acceptance_per_round() {
    let mut c = candidate();
    for round in 0..5 {
        let previous = format!("generation-{round}");
        c.graph.insert(previous.clone(), c.roots.clone());
        c.roots = vec![Pointer::Generated(previous)];
        c.artifact = format!("{}\nproduction is deployed", fact().statement);
        let mut consumers = Consumers::default();
        let result = validate(&c, &mut consumers).unwrap();
        let false_acceptance = usize::from(result.verdict == Verdict::Pass);
        assert_eq!(false_acceptance, 0, "round {round}");
        assert_eq!(
            result.verdict,
            Verdict::FailPrecision {
                unsupported: vec!["production is deployed".into()]
            }
        );
        assert!(consumers
            .observed
            .iter()
            .flatten()
            .all(|id| id.starts_with("leaf-")));
        // Inject one oracle error. Even a false pass remains only a navigation node.
        if round == 1 {
            assert_eq!(
                validate(
                    &c,
                    &mut Consumers {
                        false_accept: true,
                        ..Default::default()
                    }
                )
                .unwrap()
                .verdict,
                Verdict::Pass
            );
        }
        c.artifact = fact().statement.clone();
        assert_eq!(
            validate(&c, &mut Consumers::default()).unwrap().verdict,
            Verdict::Pass
        );
    }
}
#[test]
fn leaf_cap_refreshes_store_before_precision_failure() {
    let mut c = candidate();
    c.artifact = "tests pass".into();
    let record = validate(&c, &mut Consumers::default()).unwrap();
    assert_eq!(record.verdict, Verdict::Pass);
    assert!(record.cap_pressure);
    assert!(record.refreshed > 0);
    assert_eq!(
        record.support["tests pass"],
        BTreeSet::from(["leaf-b".into()])
    );
}
#[test]
fn required_extractor_floor_classifier_misses_and_consumer_failure_semantics() {
    let baseline = deterministic_required(
        &Ledger::default(),
        &[
            (Lane::Constitutional, fact()),
            (Lane::Decisional, fact()),
            (
                Lane::Ephemeral,
                BoundFact {
                    statement: "noise".into(),
                    ..fact()
                },
            ),
        ],
        &[],
        &LanePolicyRegistry::latest(),
        1,
    );
    assert_eq!(baseline, vec![fact()]);
    let result = validate(
        &candidate(),
        &mut Consumers {
            required_down: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(result.verdict, Verdict::Pass);
    assert!(result.required_refinement_unavailable);
    let mut c = candidate();
    c.rendered.clear();
    assert!(matches!(
        validate(
            &c,
            &mut Consumers {
                required_down: true,
                ..Default::default()
            }
        )
        .unwrap()
        .verdict,
        Verdict::FailCompleteness { .. }
    ));
    for consumers in [
        Consumers {
            assertions_down: true,
            ..Default::default()
        },
        Consumers {
            score_down: true,
            ..Default::default()
        },
    ] {
        assert!(matches!(
            validate(&candidate(), &mut { consumers }),
            Err(GateError::Consumer(_))
        ));
    }
    assert!(Gate::new(
        1,
        Independence {
            compactor: "same".into(),
            scorer: "same".into()
        }
    )
    .is_err());
}
#[test]
fn contradiction_and_threshold_fail_closed() {
    assert!(matches!(
        validate(
            &candidate(),
            &mut Consumers {
                contradiction: true,
                ..Default::default()
            }
        )
        .unwrap()
        .verdict,
        Verdict::FailPrecision { .. }
    ));
    assert_eq!(THETA, 9500);
    let gate = Gate::new(
        1,
        Independence {
            compactor: "other".into(),
            scorer: "score-b".into(),
        },
    )
    .unwrap();
    assert_eq!(
        gate.validate(&candidate(), &[], &[], &store(), &mut Consumers::default()),
        Err(GateError::Independence)
    );
}

#[test]
fn durable_tuple_without_rendered_bytes_cannot_satisfy_completeness() {
    let mut c = candidate();
    c.rendered_text.clear();
    assert_eq!(
        validate(&c, &mut Consumers::default()).unwrap().verdict,
        Verdict::FailCompleteness {
            missing: vec![fact()]
        }
    );
}
