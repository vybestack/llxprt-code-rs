use super::*;
use crate::context_kernel::{canonical::Sink, scopes::ScopeRegistry};
use std::collections::{BTreeMap, BTreeSet};

/// Read-only domain state. Mutation is crate-private to the sequenced reducer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ledger {
    pub(super) entries: BTreeMap<FactId, Entry>,
    pub(super) conflicts: BTreeSet<Conflict>,
    pub(super) versions: BTreeMap<String, u64>,
    pub(super) tool_results: BTreeSet<String>,
    pub(super) params: LedgerParams,
    pub(super) attempts: u32,
}
impl Default for Ledger {
    fn default() -> Self {
        Self::new(LedgerParams::default()).expect("valid ledger defaults")
    }
}
impl Ledger {
    pub fn new(params: LedgerParams) -> Result<Self, LedgerError> {
        if !params.threshold.valid() || params.decay_interval == 0 || params.version != 1 {
            return Err(LedgerError::Parameters);
        }
        Ok(Self {
            entries: BTreeMap::new(),
            conflicts: BTreeSet::new(),
            versions: BTreeMap::new(),
            tool_results: BTreeSet::new(),
            params,
            attempts: 0,
        })
    }
    pub fn entry(&self, fact: &FactId) -> Option<&Entry> {
        self.entries.get(fact)
    }
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values()
    }
    pub fn conflicts(&self) -> &BTreeSet<Conflict> {
        &self.conflicts
    }
    pub fn versions(&self) -> &BTreeMap<String, u64> {
        &self.versions
    }
    pub fn params(&self) -> &LedgerParams {
        &self.params
    }
    /// Single ledger-active definition, shared by all read-side consumers.
    pub fn active(&self, entry: &Entry) -> bool {
        !entry.retired
            && entry.freshness != Freshness::Unknown
            && (entry.class == FactClass::Obligation || entry.utility.0 > self.params.threshold.0)
    }
    /// Age-independent logged access/reuse normalization, saturating in `[0,1]`.
    pub fn utility(&self, usage: Usage) -> Utility {
        let score = u64::from(usage.accesses) * u64::from(self.params.access_weight)
            + u64::from(usage.reuses) * u64::from(self.params.reuse_weight);
        let decay = u64::from(usage.age_windows) * u64::from(self.params.decay_weight);
        Utility(score.saturating_sub(decay).min(u64::from(Utility::ONE)) as u16)
    }
    pub fn encode(&self, sink: &mut Sink) {
        sink.tag("fact-ledger");
        // BTree order and integer-only records make this encoding deterministic.
        sink.blob(&serde_json::to_vec(&self.params).expect("integer ledger params encode"));
        sink.int(self.attempts.into());
        for entry in self.entries.values() {
            sink.blob(&serde_json::to_vec(entry).expect("integer ledger entry encodes"));
        }
        sink.blob(&serde_json::to_vec(&self.conflicts).expect("conflicts encode"));
        sink.blob(&serde_json::to_vec(&self.versions).expect("versions encode"));
        sink.blob(&serde_json::to_vec(&self.tool_results).expect("result ids encode"));
    }
    /// Every command is atomic, including validation failures during replay.
    pub(crate) fn apply(
        &mut self,
        command: &Command,
        sequence: u64,
        scopes: &ScopeRegistry,
    ) -> Result<(), LedgerError> {
        let mut next = self.clone();
        next.transition(command, sequence, scopes)?;
        *self = next;
        Ok(())
    }
    pub(super) fn live(&self, fact: &FactId) -> Result<&Entry, LedgerError> {
        let entry = self.entry(fact).ok_or(LedgerError::Missing)?;
        if entry.retired {
            return Err(LedgerError::Identity);
        }
        Ok(entry)
    }
    fn transition(
        &mut self,
        command: &Command,
        sequence: u64,
        scopes: &ScopeRegistry,
    ) -> Result<(), LedgerError> {
        match command {
            Command::AdmitObligation { entry, grammar } => {
                self.admit_obligation(entry, *grammar, scopes)
            }
            Command::AdmitConvenience { entry } => self.admit_convenience(entry, scopes),
            Command::FactInvalidate { result, writes } => self.invalidate(result, writes),
            Command::Revalidate { observation } => self.revalidate(observation),
            Command::StaleDemote { domains } => {
                self.stale_demote(domains);
                Ok(())
            }
            Command::Redact { leaf } => {
                self.redact(leaf);
                Ok(())
            }
            Command::Decay { fact, usage } => self.decay(fact, *usage, sequence),
            Command::Demote { fact, reclaim } => self.demote(fact, *reclaim),
            Command::Discharge { fact, evidence } => self.discharge(fact, evidence, scopes),
            Command::Consolidate {
                facts,
                composite,
                support,
                reclaim,
            } => self.consolidate(facts, composite, support, *reclaim, scopes),
            Command::Amend {
                fact,
                replacement,
                principal,
            } => self.amend(fact, replacement, principal, scopes),
            Command::Retract { fact, principal } => self.retract(fact, principal),
            Command::Promote {
                fact,
                source,
                adoption,
                support,
            } => self.promote(fact, source, adoption.as_ref(), support),
        }
    }
}
