//! Classed proposal queues for the Phase-4 policy plane.
//!
//! Policy is proposal-only: nothing here mutates store or executor state.
//! Queues are classed (Safety, Ingress, Model, Controller, Monitor) with
//! reserved service shares so every nonempty class receives its reserved share
//! over a service cycle. Safety preempts, service is deterministic (and thus
//! loggable), dedup is semantic, and reproposal is monotonic and bounded.

use std::collections::BTreeMap;

/// Service classes in fixed order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum QueueClass {
    Safety,
    Ingress,
    Model,
    Controller,
    Monitor,
}

impl QueueClass {
    /// Fixed class order (deterministic service and logging).
    pub fn all() -> [QueueClass; 5] {
        [
            QueueClass::Safety,
            QueueClass::Ingress,
            QueueClass::Model,
            QueueClass::Controller,
            QueueClass::Monitor,
        ]
    }

    fn default_share(self) -> usize {
        match self {
            QueueClass::Safety => 1,
            QueueClass::Ingress => 1,
            QueueClass::Model => 1,
            QueueClass::Controller => 1,
            QueueClass::Monitor => 1,
        }
    }
}

/// A byte range inside a source buffer.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash, Default)]
pub struct SourceRange {
    pub start: u64,
    pub len: u32,
}

impl SourceRange {
    pub fn new(start: u64, len: u32) -> Self {
        Self { start, len }
    }
}

/// Semantic dedup key: operation class + source ranges + source version.
///
/// Never includes caller identity: two callers proposing the same semantic
/// operation on the same source version are the same proposal.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct SemanticKey {
    pub class: QueueClass,
    pub ranges: Vec<SourceRange>,
    pub source_version: u64,
}

impl SemanticKey {
    pub fn new(class: QueueClass, ranges: Vec<SourceRange>, source_version: u64) -> Self {
        Self {
            class,
            ranges,
            source_version,
        }
    }
}

/// A queued proposal. Priority is advisory only and never part of identity.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Proposal {
    pub key: SemanticKey,
    pub priority: u8,
    pub tokens: u64,
}

impl Proposal {
    pub fn new(key: SemanticKey, tokens: u64) -> Self {
        Self {
            key,
            priority: 0,
            tokens,
        }
    }
}

/// Queue tuning: reserved shares per class, service cycle size, retry bound.
#[derive(Clone, Debug)]
pub struct QueueConfig {
    pub cycle_slots: usize,
    pub max_retries: u32,
    pub shares: BTreeMap<QueueClass, usize>,
}

impl Default for QueueConfig {
    fn default() -> Self {
        let mut shares = BTreeMap::new();
        for class in QueueClass::all() {
            shares.insert(class, class.default_share());
        }
        Self {
            cycle_slots: 5,
            max_retries: 4,
            shares,
        }
    }
}

/// Closed, ordered view of the operation registry used by `find_admissible`.
///
/// The registry itself is owned by `context_txn::operation`; this snapshot
/// preserves its closed order for the deterministic bounded admissibility
/// search without reaching into that module.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RegistryEntry {
    pub name: &'static str,
    pub admissible: bool,
}

impl RegistryEntry {
    pub const fn new(name: &'static str, admissible: bool) -> Self {
        Self { name, admissible }
    }
}

/// Ordered registry snapshot (closed set, fixed order).
pub const REGISTRY: [RegistryEntry; 24] = [
    RegistryEntry::new("noop", false),
    RegistryEntry::new("snapshot", true),
    RegistryEntry::new("append", true),
    RegistryEntry::new("prepend", true),
    RegistryEntry::new("replace_range", true),
    RegistryEntry::new("erase_range", false),
    RegistryEntry::new("insert_placeholder", true),
    RegistryEntry::new("collapse_placeholder", true),
    RegistryEntry::new("fold_away_ephemeral", true),
    RegistryEntry::new("fold", true),
    RegistryEntry::new("compact", true),
    RegistryEntry::new("condense", true),
    RegistryEntry::new("digest_span", false),
    RegistryEntry::new("reread_span", true),
    RegistryEntry::new("reacquire_span", true),
    RegistryEntry::new("annotate", true),
    RegistryEntry::new("pin", false),
    RegistryEntry::new("unpin", true),
    RegistryEntry::new("validate", true),
    RegistryEntry::new("reorder", false),
    RegistryEntry::new("handoff", true),
    RegistryEntry::new("wrap_up", true),
    RegistryEntry::new("quiesce", true),
    RegistryEntry::new("terminal", false),
];

/// Deterministic bounded admissibility search over a caller-supplied view.
///
/// Scans in registry order and stops after `budget` inspected rows.
pub fn find_admissible_in(entries: &[RegistryEntry], budget: usize) -> Option<RegistryEntry> {
    entries
        .iter()
        .take(budget)
        .copied()
        .find(|entry| entry.admissible)
}

/// Deterministic bounded admissibility search over the closed registry view.
pub fn find_admissible(budget: usize) -> Option<RegistryEntry> {
    find_admissible_in(&REGISTRY, budget)
}

/// Classed proposal queues with reserved service shares.
#[derive(Debug)]
pub struct ClassedQueues {
    config: QueueConfig,
    queues: BTreeMap<QueueClass, Vec<Proposal>>,
    retries: BTreeMap<SemanticKey, u32>,
    served: BTreeMap<SemanticKey, u32>,
}

impl ClassedQueues {
    pub fn new(config: QueueConfig) -> Self {
        let mut queues = BTreeMap::new();
        for class in QueueClass::all() {
            queues.insert(class, Vec::new());
        }
        Self {
            config,
            queues,
            retries: BTreeMap::new(),
            served: BTreeMap::new(),
        }
    }

    pub fn config(&self) -> &QueueConfig {
        &self.config
    }

    pub fn len(&self, class: QueueClass) -> usize {
        self.queues.get(&class).map(|q| q.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.queues.values().all(|q| q.is_empty())
    }

    /// Submit a proposal. Returns `true` when newly queued, `false` when the
    /// proposal is a semantic duplicate (retry accounting still advances).
    pub fn submit(&mut self, proposal: Proposal) -> bool {
        if let Some(count) = self.retries.get_mut(&proposal.key) {
            *count = (*count).saturating_add(1).min(self.config.max_retries);
            return false;
        }
        self.retries.insert(proposal.key.clone(), 0);
        self.queues
            .entry(proposal.key.class)
            .or_default()
            .push(proposal);
        true
    }

    /// Service one cycle: deterministic, share-respecting, safety preempting.
    pub fn service_cycle(&mut self) -> Vec<Proposal> {
        let mut out = Vec::new();
        let slots = self.config.cycle_slots;
        for pass in 0..2 {
            for class in QueueClass::all() {
                let remaining = slots.saturating_sub(out.len());
                let limit = if pass == 0 {
                    (self.config.shares[&class]).min(remaining)
                } else {
                    remaining
                };
                let mut popped = Vec::new();
                if let Some(queue) = self.queues.get_mut(&class) {
                    while popped.len() < limit {
                        match queue.pop_front() {
                            Some(proposal) => popped.push(proposal),
                            None => break,
                        }
                    }
                }
                for p in popped {
                    let counter = self.served.entry(p.key.clone()).or_insert(0);
                    *counter = counter.saturating_add(1);
                    out.push(p);
                }
                if out.len() >= slots {
                    break;
                }
            }
            if out.len() >= slots {
                break;
            }
        }
        out
    }

    /// Monotonic, bounded reproposal counter for a semantic key.
    pub fn retries_for(&self, key: &SemanticKey) -> u32 {
        self.retries.get(key).copied().unwrap_or(0)
    }
}

trait PopFront<T> {
    fn pop_front(&mut self) -> Option<T>;
}

impl<T> PopFront<T> for Vec<T> {
    fn pop_front(&mut self) -> Option<T> {
        if self.is_empty() {
            None
        } else {
            Some(self.remove(0))
        }
    }
}
