//! Classed, deterministic proposal queues for the Phase-4 policy plane.
//!
//! Policy is proposal-only: this module never mutates store or executor state.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::context_txn::operation::{self, Operation};

/// Service classes in fixed preemption order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum QueueClass {
    Safety,
    Ingress,
    Model,
    Controller,
    Monitor,
}

impl QueueClass {
    pub const fn all() -> [QueueClass; 5] {
        [
            Self::Safety,
            Self::Ingress,
            Self::Model,
            Self::Controller,
            Self::Monitor,
        ]
    }

    const fn default_share(self) -> usize {
        let _ = self;
        1
    }
}

/// A byte range inside a source buffer.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash, Default)]
pub struct SourceRange {
    pub start: u64,
    pub len: u32,
}

impl SourceRange {
    pub const fn new(start: u64, len: u32) -> Self {
        Self { start, len }
    }
}

/// Validated identity of an operation in the closed transaction registry.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct OperationClass(u8);

impl OperationClass {
    pub fn from_name(name: &str) -> Option<Self> {
        operation::registry()
            .iter()
            .position(|row| row.name == name)
            .and_then(|index| u8::try_from(index).ok())
            .map(Self)
    }

    pub fn name(self) -> &'static str {
        operation::registry()[usize::from(self.0)].name
    }
}

/// Semantic identity is independent of queue service class and caller identity.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct SemanticKey {
    pub operation: OperationClass,
    pub ranges: Vec<SourceRange>,
    pub source_version: u64,
}

impl SemanticKey {
    pub fn new(operation: OperationClass, ranges: Vec<SourceRange>, source_version: u64) -> Self {
        Self {
            operation,
            ranges,
            source_version,
        }
    }
}

/// A proposal plus its deterministic service class.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Proposal {
    pub service_class: QueueClass,
    pub key: SemanticKey,
    pub priority: u8,
    pub tokens: u64,
}

impl Proposal {
    pub fn new(service_class: QueueClass, key: SemanticKey, tokens: u64) -> Self {
        Self {
            service_class,
            key,
            priority: 0,
            tokens,
        }
    }
}

/// Reserved shares per service cycle and the reproposal bound.
#[derive(Clone, Debug)]
pub struct QueueConfig {
    pub cycle_slots: usize,
    pub max_retries: u32,
    pub shares: BTreeMap<QueueClass, usize>,
}

impl Default for QueueConfig {
    fn default() -> Self {
        let shares = QueueClass::all()
            .into_iter()
            .map(|class| (class, class.default_share()))
            .collect();
        Self {
            cycle_slots: 5,
            max_retries: 4,
            shares,
        }
    }
}

/// Search the actual closed transaction registry in its declared order.
pub fn find_admissible(
    budget: usize,
    mut admissible: impl FnMut(&Operation) -> bool,
) -> Option<&'static Operation> {
    operation::registry()
        .iter()
        .take(budget)
        .find(|row| admissible(row))
}

/// Classed proposal queues with semantic dedup and bounded reproposal.
#[derive(Debug)]
pub struct ClassedQueues {
    config: QueueConfig,
    queues: BTreeMap<QueueClass, VecDeque<Proposal>>,
    pending: BTreeSet<SemanticKey>,
    retries: BTreeMap<SemanticKey, u32>,
    served: BTreeMap<SemanticKey, u32>,
}

impl ClassedQueues {
    pub fn new(config: QueueConfig) -> Self {
        let all_shares_present = QueueClass::all()
            .into_iter()
            .all(|class| config.shares.contains_key(&class));
        assert!(
            all_shares_present,
            "every queue class needs a reserved share"
        );
        let reserved: usize = config.shares.values().copied().sum();
        assert!(
            reserved <= config.cycle_slots,
            "reserved shares exceed cycle slots"
        );
        let queues = QueueClass::all()
            .into_iter()
            .map(|class| (class, VecDeque::new()))
            .collect();
        Self {
            config,
            queues,
            pending: BTreeSet::new(),
            retries: BTreeMap::new(),
            served: BTreeMap::new(),
        }
    }

    pub fn config(&self) -> &QueueConfig {
        &self.config
    }

    pub fn len(&self, class: QueueClass) -> usize {
        self.queues.get(&class).map(VecDeque::len).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Insert a new semantic proposal. A pending or previously served key is a duplicate.
    pub fn submit(&mut self, proposal: Proposal) -> bool {
        if self.retries.contains_key(&proposal.key) {
            return false;
        }
        self.retries.insert(proposal.key.clone(), 0);
        self.pending.insert(proposal.key.clone());
        self.queues
            .get_mut(&proposal.service_class)
            .expect("validated queue configuration contains every class")
            .push_back(proposal);
        true
    }

    /// Requeue a served proposal while its monotonic retry budget remains.
    pub fn repropose(&mut self, proposal: Proposal) -> bool {
        if self.pending.contains(&proposal.key) {
            return false;
        }
        let Some(retries) = self.retries.get_mut(&proposal.key) else {
            return false;
        };
        if *retries >= self.config.max_retries {
            return false;
        }
        *retries = retries.saturating_add(1);
        self.pending.insert(proposal.key.clone());
        self.queues
            .get_mut(&proposal.service_class)
            .expect("validated queue configuration contains every class")
            .push_back(proposal);
        true
    }

    /// Service reserved shares first, then fill deterministically in preemption order.
    pub fn service_cycle(&mut self) -> Vec<Proposal> {
        let mut out = Vec::new();
        for pass in 0..2 {
            for class in QueueClass::all() {
                let remaining = self.config.cycle_slots.saturating_sub(out.len());
                let limit = if pass == 0 {
                    self.config.shares[&class].min(remaining)
                } else {
                    remaining
                };
                let queue = self
                    .queues
                    .get_mut(&class)
                    .expect("validated queue configuration contains every class");
                for _ in 0..limit {
                    let Some(proposal) = queue.pop_front() else {
                        break;
                    };
                    self.pending.remove(&proposal.key);
                    let served = self.served.entry(proposal.key.clone()).or_insert(0);
                    *served = served.saturating_add(1);
                    out.push(proposal);
                }
                if out.len() == self.config.cycle_slots {
                    break;
                }
            }
            if out.len() == self.config.cycle_slots {
                break;
            }
        }
        out
    }

    pub fn retries_for(&self, key: &SemanticKey) -> u32 {
        self.retries.get(key).copied().unwrap_or(0)
    }

    pub fn served_for(&self, key: &SemanticKey) -> u32 {
        self.served.get(key).copied().unwrap_or(0)
    }
}
