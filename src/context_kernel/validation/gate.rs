use super::{provenance, *};
use crate::context_kernel::{
    lanes::{Lane, LanePolicyRegistry, SurvivalClass},
    ledger::{FactClass, Ledger},
};
use std::collections::{BTreeMap, BTreeSet};

/// The deterministic floor cannot be subtracted by a model/classifier miss.
pub fn deterministic_required(
    ledger: &Ledger,
    items: &[(Lane, BoundFact)],
    patterns: &[BoundFact],
    registry: &LanePolicyRegistry,
    scope: u64,
) -> Vec<BoundFact> {
    let mut facts: BTreeSet<_> = patterns.iter().cloned().collect();
    for (lane, fact) in items {
        let required = registry.policy(*lane).is_ok_and(|p| {
            matches!(
                p.survival,
                SurvivalClass::Protected | SurvivalClass::Required
            )
        });
        if required && (*lane == Lane::Constitutional || fact.scope == scope) {
            facts.insert(fact.clone());
        }
    }
    for entry in ledger
        .entries()
        .filter(|e| !e.retired && e.class == FactClass::Obligation)
    {
        facts.insert(BoundFact {
            statement: entry.fold.text.clone(),
            subject: entry.id.subject.clone(),
            predicate: entry.id.predicate.clone(),
            scope: entry.id.scope,
            command: entry
                .bindings
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n"),
            negated: false,
            temporal: String::new(),
        });
    }
    facts.into_iter().collect()
}

pub struct Gate {
    leaf_cap: usize,
    independence: Independence,
}
impl Gate {
    pub fn new(leaf_cap: usize, independence: Independence) -> Result<Self, GateError> {
        if leaf_cap == 0 || leaf_cap > MAX_GRAPH_NODES {
            return Err(GateError::Bounds);
        }
        if independence.compactor.is_empty()
            || independence.scorer.is_empty()
            || independence.compactor == independence.scorer
        {
            return Err(GateError::Independence);
        }
        Ok(Self {
            leaf_cap,
            independence,
        })
    }
    pub fn independence(&self) -> &Independence {
        &self.independence
    }
    pub fn validate(
        &self,
        candidate: &Candidate,
        required: &[BoundFact],
        always_checked: &[BoundFact],
        store: &dyn LeafStore,
        consumers: &mut dyn GateConsumers,
    ) -> Result<ValidationRecord, GateError> {
        if consumers.independence() != self.independence {
            return Err(GateError::Independence);
        }
        input_bounds(candidate, required, always_checked)?;
        let mut record = ValidationRecord {
            candidate_digest: candidate_digest(candidate),
            verdict: Verdict::Pass,
            support: BTreeMap::new(),
            cap_pressure: false,
            refreshed: 0,
            required_refinement_unavailable: false,
        };
        let ids = match provenance::roots(candidate) {
            Ok(ids) => ids,
            Err(reason) => {
                record.verdict = Verdict::FailProvenance(reason);
                return Ok(record);
            }
        };
        if ids
            .iter()
            .any(|id| !store.authenticated_leaf(id).is_some_and(|l| l.is_ingress()))
        {
            record.verdict = Verdict::FailProvenance(ProvenanceFailure::UngroundedRoot);
            return Ok(record);
        }
        let facts = required_union(required, consumers, &mut record);
        let missing: Vec<_> = facts
            .iter()
            .filter(|fact| {
                !candidate.rendered.contains(fact)
                    || !candidate.rendered_text.contains(&fact.statement)
            })
            .cloned()
            .collect();
        if !missing.is_empty() || !candidate.pending_notes.is_empty() {
            record.verdict = Verdict::FailCompleteness { missing };
            return Ok(record);
        }
        let assertions = extract_assertions(candidate, consumers)?;
        let contradictions: Vec<_> = facts
            .into_iter()
            .filter(|f| f.scope == candidate.scope)
            .chain(always_checked.iter().cloned())
            .collect();
        let mut unsupported = Vec::new();
        record.cap_pressure = ids.len() > self.leaf_cap;
        for assertion in assertions {
            let support = self.support(
                &assertion,
                &ids,
                &contradictions,
                store,
                consumers,
                &mut record,
            )?;
            if support.is_empty() {
                unsupported.push(assertion);
            } else {
                record.support.insert(assertion, support);
            }
        }
        if !unsupported.is_empty() {
            record.verdict = Verdict::FailPrecision { unsupported };
        }
        Ok(record)
    }
    fn support(
        &self,
        assertion: &str,
        ids: &BTreeSet<String>,
        contradictions: &[BoundFact],
        store: &dyn LeafStore,
        consumers: &mut dyn GateConsumers,
        record: &mut ValidationRecord,
    ) -> Result<BTreeSet<String>, GateError> {
        let mut support = BTreeSet::new();
        let ordered: Vec<_> = ids.iter().collect();
        for (page, chunk) in ordered.chunks(self.leaf_cap).enumerate() {
            // Refresh actual bytes/metadata from the authenticated store, not summaries.
            let leaves: Vec<_> = chunk
                .iter()
                .map(|id| {
                    store
                        .authenticated_leaf(id)
                        .filter(Leaf::is_ingress)
                        .ok_or(GateError::Consumer("leaf-store"))
                })
                .collect::<Result<_, _>>()?;
            if leaves
                .iter()
                .any(|l| l.sanitized.len() > MAX_ARTIFACT_BYTES)
            {
                return Err(GateError::Bounds);
            }
            if page > 0 {
                record.refreshed += leaves.len();
            }
            let score = consumers.score(assertion, &leaves, contradictions)?;
            if score.entailment > 10_000 {
                return Err(GateError::Bounds);
            }
            if score.contradiction {
                return Ok(BTreeSet::new());
            }
            if score.entailment >= THETA {
                support.extend(leaves.into_iter().map(|l| l.id));
            }
        }
        Ok(support)
    }
}
fn extract_assertions(
    candidate: &Candidate,
    consumers: &mut dyn GateConsumers,
) -> Result<BTreeSet<String>, GateError> {
    let extracted = consumers.assertions(&candidate.artifact)?;
    if extracted.is_empty()
        || extracted.len() > MAX_ASSERTIONS
        || extracted
            .iter()
            .any(|s| s.trim().is_empty() || s.len() > MAX_ARTIFACT_BYTES)
    {
        return Err(GateError::Bounds);
    }
    // Deterministic line floor audits extractor omissions without a semantic replacement.
    let mut assertions: BTreeSet<_> = extracted.into_iter().collect();
    assertions.extend(
        candidate
            .artifact
            .lines()
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned),
    );
    if assertions.len() > MAX_ASSERTIONS {
        return Err(GateError::Bounds);
    }
    Ok(assertions)
}

fn required_union(
    required: &[BoundFact],
    consumers: &mut dyn GateConsumers,
    record: &mut ValidationRecord,
) -> BTreeSet<BoundFact> {
    let mut facts: BTreeSet<_> = required.iter().cloned().collect();
    match consumers.required_facts(required) {
        Ok(refined) if refined.len() <= MAX_GRAPH_NODES => {
            facts.extend(refined);
        }
        _ => record.required_refinement_unavailable = true,
    }
    facts
}

fn input_bounds(
    candidate: &Candidate,
    required: &[BoundFact],
    always_checked: &[BoundFact],
) -> Result<(), GateError> {
    if candidate.artifact.is_empty()
        || candidate.artifact.len() > MAX_ARTIFACT_BYTES
        || required.len() > MAX_GRAPH_NODES
        || candidate.rendered.len() > MAX_GRAPH_NODES
        || candidate.rendered_text.len() > MAX_ENVELOPE_BYTES
        || always_checked.len() > MAX_GRAPH_NODES
    {
        return Err(GateError::Bounds);
    }
    Ok(())
}
