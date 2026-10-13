use super::*;
use std::collections::{BTreeMap, BTreeSet};

impl Ledger {
    pub(super) fn invalidate(
        &mut self,
        result: &ToolWrite,
        writes: &BTreeMap<String, u64>,
    ) -> Result<(), LedgerError> {
        if result.call_id.is_empty()
            || result.tool.is_empty()
            || self.tool_results.contains(&result.call_id)
        {
            return Err(LedgerError::Identity);
        }
        if writes
            .iter()
            .any(|(domain, version)| self.versions.get(domain).is_some_and(|old| version <= old))
        {
            return Err(LedgerError::Precondition);
        }
        self.versions.extend(writes.clone());
        self.tool_results.insert(result.call_id.clone());
        let mut invalidated = BTreeSet::new();
        for entry in self.entries.values_mut().filter(|entry| !entry.retired) {
            if entry
                .read_set
                .keys()
                .any(|domain| writes.contains_key(domain))
                && entry.freshness == Freshness::Current
            {
                entry.freshness = Freshness::Stale;
                if entry.kind == FactKind::Empirical {
                    invalidated.insert(entry.id.clone());
                }
            }
        }
        for normative in self
            .entries
            .values()
            .filter(|entry| entry.kind == FactKind::Normative && !entry.retired)
        {
            for empirical in normative.premises.intersection(&invalidated) {
                self.conflicts.insert(Conflict::PremiseInvalidated {
                    normative: normative.id.clone(),
                    empirical: empirical.clone(),
                });
            }
        }
        Ok(())
    }
    pub(super) fn revalidate(&mut self, observation: &Observation) -> Result<(), LedgerError> {
        if observation.source.origin != Origin::Ingress || observation.source.leaf.is_empty() {
            return Err(LedgerError::Unsupported);
        }
        for entry in self.entries.values_mut().filter(|entry| !entry.retired) {
            let intersects = entry
                .read_set
                .keys()
                .any(|domain| observation.read_set.contains_key(domain));
            let selected = intersects
                && observation.supports.contains(&entry.id)
                && observation.source.scope == entry.id.scope;
            if !selected || entry.freshness != Freshness::Stale {
                continue;
            }
            // Intersection selects pairs, but the complete dependency set must match.
            let matches = entry.read_set.keys().all(|domain| {
                let observed = observation.read_set.get(domain);
                observed.is_some()
                    && observed
                        == self
                            .versions
                            .get(domain)
                            .or_else(|| entry.read_set.get(domain))
            });
            if matches {
                for (domain, version) in &mut entry.read_set {
                    *version = observation.read_set[domain];
                }
                entry.freshness = Freshness::Current;
                entry.evidence = EvidenceStatus::Verified;
                // An observation refreshes evidence, never the authority receipt
                // of an existing normative decision or obligation.
                if entry.kind == FactKind::Empirical {
                    entry.source = observation.source.clone();
                }
            }
        }
        // A normative conflict stays flagged until an authorized amendment;
        // refreshing an empirical premise does not silently resolve a decision.
        Ok(())
    }
    pub(super) fn stale_demote(&mut self, domains: &BTreeSet<String>) {
        for entry in self.entries.values_mut() {
            if entry.freshness == Freshness::Stale
                && entry.read_set.keys().any(|domain| domains.contains(domain))
            {
                entry.evidence = EvidenceStatus::Unverified;
            }
        }
    }
    pub(super) fn redact(&mut self, leaf: &str) {
        for entry in self.entries.values_mut() {
            if entry.source.leaf == leaf || entry.bindings.contains(leaf) {
                entry.evidence = EvidenceStatus::Unverified;
            }
        }
    }
}
