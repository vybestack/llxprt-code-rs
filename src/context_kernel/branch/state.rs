use super::*;
use crate::context_kernel::canonical::Sink;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BranchState {
    branches: BTreeMap<Namespace, Branch>,
}
impl BranchState {
    pub fn branches(&self) -> &BTreeMap<Namespace, Branch> {
        &self.branches
    }
    pub fn active_slots(&self) -> usize {
        self.branches.values().filter(|b| b.active()).count()
    }
    pub fn encode(&self, sink: &mut Sink) {
        sink.tag("context-branches");
        sink.blob(&serde_json::to_vec(&self.branches).expect("typed branches"));
    }
    pub(crate) fn apply(
        &mut self,
        event: &BranchEvent,
        constitution: &BTreeMap<String, String>,
        versions: &BTreeMap<String, u64>,
    ) -> Result<(), BranchError> {
        let mut next = self.clone();
        match event {
            BranchEvent::Open(spawn) => next.open(spawn, constitution)?,
            BranchEvent::Round { namespace, round } => next.round(namespace, round)?,
            BranchEvent::Return {
                namespace,
                returned,
            } => {
                let branch = next.live(namespace)?;
                let conflicts = conflicts(branch, versions);
                let expected = if conflicts.is_empty() {
                    ReturnStatus::CurrentEvidence
                } else {
                    ReturnStatus::SuggestionNeedsRevalidation
                };
                if returned.handle.is_empty()
                    || !crate::context_kernel::security::valid_hash(&returned.digest)
                    || returned.status != expected
                    || returned.revalidate != conflicts
                    || branch.rounds.iter().any(|r| !r.complete)
                {
                    return Err(BranchError::Receipt);
                }
                next.no_children(namespace)?;
                next.branches.get_mut(namespace).expect("live").returned = Some(returned.clone());
            }
            BranchEvent::Abort { namespace } => {
                next.live(namespace)?;
                next.no_children(namespace)?;
                next.branches.get_mut(namespace).expect("live").aborted = true;
            }
        }
        *self = next;
        Ok(())
    }
    pub fn return_status(
        &self,
        namespace: &str,
        versions: &BTreeMap<String, u64>,
    ) -> Result<(ReturnStatus, BTreeSet<String>), BranchError> {
        let conflicts = conflicts(self.live(namespace)?, versions);
        let status = if conflicts.is_empty() {
            ReturnStatus::CurrentEvidence
        } else {
            ReturnStatus::SuggestionNeedsRevalidation
        };
        Ok((status, conflicts))
    }
    fn live(&self, namespace: &str) -> Result<&Branch, BranchError> {
        self.branches
            .get(namespace)
            .filter(|b| b.active())
            .ok_or(BranchError::Namespace)
    }
    fn no_children(&self, namespace: &str) -> Result<(), BranchError> {
        if self
            .branches
            .values()
            .any(|b| b.active() && b.spawn.parent.as_deref() == Some(namespace))
        {
            return Err(BranchError::Namespace);
        }
        Ok(())
    }
    fn check_limits(&self, limits: &Limits) -> Result<(), BranchError> {
        let defaults = Limits::default();
        if limits.depth == 0
            || limits.depth > defaults.depth
            || limits.concurrency == 0
            || limits.concurrency > defaults.concurrency
            || limits.context == 0
            || limits.context > defaults.context
            || limits.management == 0
            || limits.management > defaults.management
            || self.active_slots() >= limits.concurrency
            || self
                .branches
                .values()
                .any(|b| b.active() && self.active_slots() >= b.spawn.limits.concurrency)
        {
            return Err(BranchError::Limit);
        }
        Ok(())
    }
    fn open(
        &mut self,
        spawn: &Spawn,
        constitution: &BTreeMap<String, String>,
    ) -> Result<(), BranchError> {
        if spawn.namespace.is_empty()
            || spawn.namespace.len() > 256
            || spawn.parent_version == 0
            || self.branches.contains_key(&spawn.namespace)
            || self.branches.len() >= 4096
        {
            return Err(BranchError::Namespace);
        }
        if &spawn.constitutional != constitution {
            return Err(BranchError::Constitution);
        }
        if spawn.tools.is_empty()
            || spawn.tools.iter().any(|t| !t.permits_branch())
            || spawn
                .tools
                .iter()
                .map(|t| &t.name)
                .collect::<BTreeSet<_>>()
                .len()
                != spawn.tools.len()
        {
            return Err(BranchError::Tool);
        }
        let limits = &spawn.limits;
        self.check_limits(limits)?;
        let depth = if let Some(parent) = &spawn.parent {
            let parent = self.live(parent)?;
            if limits.depth > parent.spawn.limits.depth
                || limits.concurrency > parent.spawn.limits.concurrency
                || limits.context > parent.spawn.limits.context
                || limits.management > parent.spawn.limits.management
            {
                return Err(BranchError::Limit);
            }
            parent.depth + 1
        } else {
            1
        };
        if depth > limits.depth {
            return Err(BranchError::Limit);
        }
        self.branches.insert(
            spawn.namespace.clone(),
            Branch {
                spawn: spawn.clone(),
                depth,
                rounds: vec![],
                reads: BTreeMap::new(),
                context_spend: 0,
                management_spend: 0,
                returned: None,
                aborted: false,
            },
        );
        Ok(())
    }
    fn round(&mut self, namespace: &str, round: &Round) -> Result<(), BranchError> {
        let branch = self.live(namespace)?;
        let tool = branch
            .spawn
            .tools
            .iter()
            .find(|t| t.name == round.tool)
            .ok_or(BranchError::Tool)?;
        if !tool.permits_branch() {
            return Err(BranchError::Tool);
        }
        if round.call.is_empty()
            || round.store_handle.is_empty()
            || branch.rounds.len() >= 4096
            || round.reads.keys().cloned().collect::<BTreeSet<_>>() != tool.reads
            || branch.rounds.iter().any(|r| r.call == round.call)
            || branch
                .reads
                .iter()
                .any(|(d, v)| round.reads.get(d).is_some_and(|new| new != v))
        {
            return Err(BranchError::Receipt);
        }
        let context = branch
            .context_spend
            .checked_add(round.context_spend)
            .ok_or(BranchError::Limit)?;
        let management = branch
            .management_spend
            .checked_add(round.management_spend)
            .ok_or(BranchError::Limit)?;
        if context > branch.spawn.limits.context || management > branch.spawn.limits.management {
            return Err(BranchError::Limit);
        }
        let branch = self.branches.get_mut(namespace).expect("live");
        branch.context_spend = context;
        branch.management_spend = management;
        branch.reads.extend(round.reads.clone());
        branch.rounds.push(round.clone());
        Ok(())
    }
}
fn conflicts(branch: &Branch, versions: &BTreeMap<String, u64>) -> BTreeSet<String> {
    branch
        .reads
        .iter()
        .filter(|(d, v)| versions.get(*d).copied().unwrap_or(0) != **v)
        .map(|(d, _)| d.clone())
        .collect()
}
