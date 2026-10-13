use super::*;
use crate::context_kernel::scopes::ScopeRegistry;
use std::collections::BTreeSet;

pub(super) fn supported(support: &GateSupport, fact: &FactId, claim: &str) -> bool {
    support.fact == *fact
        && !claim.is_empty()
        && support.claim == claim
        && !support.ingress_leaves.is_empty()
        && support.ingress_leaves.iter().all(|leaf| !leaf.is_empty())
}
fn authorized(principal: &Source, entry: &Entry) -> bool {
    principal.origin == Origin::Ingress
        && !principal.leaf.is_empty()
        && principal.scope == entry.id.scope
        && principal.authority >= entry.source.authority
}
impl Ledger {
    pub(super) fn demote(&mut self, fact: &FactId, reclaim: u64) -> Result<(), LedgerError> {
        let entry = self.live(fact)?;
        if entry.class == FactClass::Obligation {
            return Err(LedgerError::ObligationProtected);
        }
        if !entry.rendered || reclaim < 8 || reclaim > entry.fold.text.len() as u64 {
            return Err(LedgerError::Precondition);
        }
        self.entries.get_mut(fact).expect("live entry").rendered = false;
        Ok(())
    }
    pub(super) fn decay(
        &mut self,
        fact: &FactId,
        usage: Usage,
        sequence: u64,
    ) -> Result<(), LedgerError> {
        let entry = self.live(fact)?;
        if entry.class == FactClass::Obligation {
            return Err(LedgerError::ObligationProtected);
        }
        if sequence.saturating_sub(entry.last_decay) < self.params.decay_interval
            || usage.age_windows <= entry.usage.age_windows
        {
            return Err(LedgerError::Precondition);
        }
        if usage.accesses != entry.usage.accesses || usage.reuses != entry.usage.reuses {
            return Err(LedgerError::Precondition);
        }
        let utility = self.utility(usage);
        let entry = self.entries.get_mut(fact).expect("live entry");
        entry.utility = utility;
        entry.usage = usage;
        entry.last_decay = sequence;
        Ok(())
    }
    pub(super) fn discharge(
        &mut self,
        fact: &FactId,
        evidence: &DischargeEvidence,
        scopes: &ScopeRegistry,
    ) -> Result<(), LedgerError> {
        let entry = self.live(fact)?;
        if entry.class != FactClass::Obligation {
            return Err(LedgerError::Precondition);
        }
        let allowed = match evidence {
            DischargeEvidence::Insufficient => return Err(LedgerError::Insufficient),
            DischargeEvidence::Resolved(support) => supported(support, fact, &support.claim),
            DischargeEvidence::SupersededBy(successor) => {
                successor != fact
                    && self.live(successor).is_ok_and(|next| {
                        next.class == FactClass::Obligation
                            && next.id.subject == fact.subject
                            && next.id.scope == fact.scope
                            && next.source.authority >= entry.source.authority
                            && next.bindings.is_superset(&entry.bindings)
                    })
            }
            DischargeEvidence::ScopeClosed => {
                scopes.state(fact.scope).is_ok_and(|scope| !scope.is_open())
            }
        };
        if !allowed {
            return Err(LedgerError::Unsupported);
        }
        if let DischargeEvidence::SupersededBy(successor) = evidence {
            let text = self.live(successor)?.fold.text.clone();
            let old = self.entries.get_mut(fact).expect("live entry");
            old.superseded_by = Some(successor.clone());
            old.fold.text = text;
        }
        self.entries.get_mut(fact).expect("live entry").retired = true;
        self.remove_conflicts(fact);
        Ok(())
    }
    pub(super) fn amend(
        &mut self,
        fact: &FactId,
        replacement: &Entry,
        principal: &Source,
        scopes: &ScopeRegistry,
    ) -> Result<(), LedgerError> {
        let old = self.live(fact)?.clone();
        if old.class == FactClass::Obligation {
            return Err(LedgerError::ObligationProtected);
        }
        if !authorized(principal, &old) || replacement.source.authority > principal.authority {
            return Err(LedgerError::Authority);
        }
        if replacement.class != old.class
            || replacement.kind != old.kind
            || replacement.id.scope != fact.scope
            || replacement.fold.depth != old.fold.depth
        {
            return Err(LedgerError::Precondition);
        }
        if replacement.kind == FactKind::Normative
            && (replacement.source.origin != Origin::Ingress
                || replacement.source.authority < Authority::Agent)
        {
            return Err(LedgerError::Authority);
        }
        self.check_entry(replacement, scopes)?;
        // The old carrying projection is rewritten here in the same commit;
        // rendering never overlays an obsolete sentence and depth never grows.
        let carrying = self.entries.get_mut(fact).expect("live entry");
        carrying.fold.text = replacement.fold.text.clone();
        carrying.superseded_by = Some(replacement.id.clone());
        carrying.retired = true;
        self.entries
            .insert(replacement.id.clone(), replacement.clone());
        self.remove_conflicts(fact);
        self.record_conflicts(replacement);
        Ok(())
    }
    pub(super) fn retract(&mut self, fact: &FactId, principal: &Source) -> Result<(), LedgerError> {
        let entry = self.live(fact)?;
        if entry.class == FactClass::Obligation {
            return Err(LedgerError::ObligationProtected);
        }
        if !authorized(principal, entry) {
            return Err(LedgerError::Authority);
        }
        let entry = self.entries.get_mut(fact).expect("live entry");
        entry.fold.text.clear();
        entry.rendered = false;
        entry.retired = true;
        self.remove_conflicts(fact);
        Ok(())
    }
    pub(super) fn promote(
        &mut self,
        fact: &FactId,
        source: &Source,
        adoption: Option<&Source>,
        support: &GateSupport,
    ) -> Result<(), LedgerError> {
        let entry = self.live(fact)?;
        if entry.class == FactClass::Obligation {
            return Err(LedgerError::ObligationProtected);
        }
        let required = entry.source.authority.max(Authority::Agent);
        let same_source = source.origin == Origin::Ingress
            && source.authority >= required
            && source.scope == fact.scope
            && source.leaf == entry.source.leaf;
        let adopted = adoption.is_some_and(|principal| {
            principal.origin == Origin::Ingress
                && principal.authority >= required
                && principal.scope == fact.scope
                && !principal.leaf.is_empty()
        });
        if !same_source && !adopted {
            return Err(LedgerError::Authority);
        }
        if !supported(support, fact, &entry.fold.text) {
            return Err(LedgerError::Unsupported);
        }
        let entry = self.entries.get_mut(fact).expect("live entry");
        entry.kind = FactKind::Normative;
        entry.source = adoption.unwrap_or(source).clone();
        Ok(())
    }
    pub(super) fn consolidate(
        &mut self,
        facts: &BTreeSet<FactId>,
        composite: &Entry,
        support: &GateSupport,
        reclaim: u64,
        scopes: &ScopeRegistry,
    ) -> Result<(), LedgerError> {
        if self.attempts >= self.params.consolidation_attempts || facts.len() < 2 {
            return Err(LedgerError::Overflow);
        }
        let mut bindings = BTreeSet::new();
        let mut bytes = 0u64;
        for fact in facts {
            let entry = self.live(fact)?;
            if entry.class != FactClass::Obligation
                || entry.id.scope != composite.id.scope
                || composite.source.authority < entry.source.authority
            {
                return Err(LedgerError::Authority);
            }
            bindings.extend(entry.bindings.clone());
            bytes += entry.fold.text.len() as u64;
        }
        if bindings != composite.bindings
            || reclaim < 8
            || bytes.saturating_sub(composite.fold.text.len() as u64) < reclaim
        {
            return Err(LedgerError::Precondition);
        }
        if !supported(support, &composite.id, &composite.fold.text) {
            return Err(LedgerError::Unsupported);
        }
        let live_bytes: u64 = self
            .entries
            .values()
            .filter(|entry| !entry.retired && entry.class == FactClass::Obligation)
            .map(|entry| entry.fold.text.len() as u64)
            .sum();
        if live_bytes < self.params.protection_bytes
            && self.entries.values().filter(|entry| !entry.retired).count() < self.params.capacity
        {
            return Err(LedgerError::Precondition);
        }
        self.attempts += 1;
        for fact in facts {
            let entry = self.entries.get_mut(fact).expect("checked constituent");
            entry.retired = true;
            entry.superseded_by = Some(composite.id.clone());
            entry.fold.text = composite.fold.text.clone();
            self.remove_conflicts(fact);
        }
        self.admit_obligation(composite, Grammar::Obligation, scopes)
    }
}
