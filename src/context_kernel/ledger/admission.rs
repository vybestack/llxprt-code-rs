use super::*;
use crate::context_kernel::scopes::ScopeRegistry;

impl Ledger {
    pub(super) fn check_entry(
        &self,
        entry: &Entry,
        scopes: &ScopeRegistry,
    ) -> Result<(), LedgerError> {
        if self.entries.contains_key(&entry.id) || entry.retired || entry.superseded_by.is_some() {
            return Err(LedgerError::Identity);
        }
        if !entry.utility.valid() || entry.utility != self.utility(entry.usage) {
            return Err(LedgerError::Parameters);
        }
        if entry.source.scope != entry.id.scope || entry.source.leaf.is_empty() {
            return Err(LedgerError::Authority);
        }
        if !scopes
            .state(entry.id.scope)
            .is_ok_and(|scope| scope.is_open())
        {
            return Err(LedgerError::Precondition);
        }
        if entry.freshness == Freshness::Current && entry.evidence != EvidenceStatus::Verified {
            return Err(LedgerError::Unsupported);
        }
        if self.entries.values().filter(|entry| !entry.retired).count() >= self.params.capacity {
            return Err(LedgerError::Overflow);
        }
        Ok(())
    }
    pub(super) fn admit_obligation(
        &mut self,
        entry: &Entry,
        grammar: Grammar,
        scopes: &ScopeRegistry,
    ) -> Result<(), LedgerError> {
        if entry.source.authority == Authority::Environment || grammar == Grammar::Evidence {
            return Err(LedgerError::Quarantined);
        }
        if entry.source.origin != Origin::Ingress
            || entry.class != FactClass::Obligation
            || entry.kind != FactKind::Normative
        {
            return Err(LedgerError::Authority);
        }
        let ambiguous = grammar == Grammar::Ambiguous;
        if ambiguous && entry.source.authority < Authority::User {
            return Err(LedgerError::Authority);
        }
        if entry.unconfirmed != ambiguous {
            return Err(LedgerError::Precondition);
        }
        self.check_entry(entry, scopes)?;
        self.check_obligation_bound(entry)?;
        self.record_conflicts(entry);
        self.entries.insert(entry.id.clone(), entry.clone());
        Ok(())
    }
    fn check_obligation_bound(&self, entry: &Entry) -> Result<(), LedgerError> {
        let live = self
            .entries
            .values()
            .filter(|entry| !entry.retired && entry.class == FactClass::Obligation);
        let bytes = live
            .clone()
            .map(|entry| entry.fold.text.len() as u64)
            .sum::<u64>();
        if bytes.saturating_add(entry.fold.text.len() as u64) > self.params.protection_bytes {
            return Err(LedgerError::Overflow);
        }
        if entry.unconfirmed
            && live.filter(|entry| entry.unconfirmed).count() >= self.params.unconfirmed_capacity
        {
            return Err(LedgerError::Overflow);
        }
        Ok(())
    }
    pub(super) fn admit_convenience(
        &mut self,
        entry: &Entry,
        scopes: &ScopeRegistry,
    ) -> Result<(), LedgerError> {
        self.check_entry(entry, scopes)?;
        if entry.class != FactClass::Convenience
            || entry.utility.0 <= self.params.threshold.0
            || entry.unconfirmed
        {
            return Err(LedgerError::Precondition);
        }
        if entry.kind == FactKind::Normative
            && (entry.source.authority < Authority::Agent || entry.source.origin != Origin::Ingress)
        {
            return Err(LedgerError::Authority);
        }
        self.record_conflicts(entry);
        self.entries.insert(entry.id.clone(), entry.clone());
        Ok(())
    }
    /// No precedence rule changes a normative entry. The ordered matrix flags
    /// incompatible assertions with the same subject/scope for explicit repair.
    pub(super) fn record_conflicts(&mut self, incoming: &Entry) {
        for entry in self.entries.values().filter(|entry| !entry.retired) {
            if entry.id.subject != incoming.id.subject
                || entry.id.scope != incoming.id.scope
                || entry.fold.text == incoming.fold.text
            {
                continue;
            }
            let (left, right) = if entry.id < incoming.id {
                (entry.id.clone(), incoming.id.clone())
            } else {
                (incoming.id.clone(), entry.id.clone())
            };
            let conflict = match (entry.kind, incoming.kind) {
                (FactKind::Normative, FactKind::Normative) => Conflict::Normative { left, right },
                (FactKind::Empirical, FactKind::Empirical) => Conflict::Empirical { left, right },
                (FactKind::Normative, FactKind::Empirical) => Conflict::PremiseInvalidated {
                    normative: entry.id.clone(),
                    empirical: incoming.id.clone(),
                },
                (FactKind::Empirical, FactKind::Normative) => Conflict::PremiseInvalidated {
                    normative: incoming.id.clone(),
                    empirical: entry.id.clone(),
                },
            };
            self.conflicts.insert(conflict);
        }
    }
    pub(super) fn remove_conflicts(&mut self, fact: &FactId) {
        self.conflicts.retain(|conflict| match conflict {
            Conflict::Normative { left, right } | Conflict::Empirical { left, right } => {
                left != fact && right != fact
            }
            Conflict::PremiseInvalidated {
                normative,
                empirical,
            } => normative != fact && empirical != fact,
        });
    }
}
