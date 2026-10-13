use super::{Candidate, Pointer, ProvenanceFailure, MAX_GRAPH_NODES};
use std::collections::{BTreeSet, VecDeque};

/// Iterative DFS: bounded even for adversarially deep/cyclic summary chains.
pub(super) fn roots(candidate: &Candidate) -> Result<BTreeSet<String>, ProvenanceFailure> {
    let mut leaves = BTreeSet::new();
    let mut active = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut stack: VecDeque<(Pointer, bool)> = candidate
        .roots
        .iter()
        .cloned()
        .map(|p| (p, false))
        .collect();
    let edges: usize = candidate.graph.values().map(Vec::len).sum();
    if candidate.graph.len() > MAX_GRAPH_NODES
        || edges > MAX_GRAPH_NODES
        || stack.len() > MAX_GRAPH_NODES
    {
        return Err(ProvenanceFailure::UngroundedRoot);
    }
    while let Some((pointer, exit)) = stack.pop_back() {
        match pointer {
            Pointer::Ingress(id) => {
                leaves.insert(id);
            }
            Pointer::Generated(id) => {
                if exit {
                    active.remove(&id);
                    visited.insert(id);
                    continue;
                }
                if active.contains(&id) {
                    return Err(ProvenanceFailure::Cycle);
                }
                if visited.contains(&id) {
                    continue;
                }
                let children = candidate
                    .graph
                    .get(&id)
                    .filter(|c| !c.is_empty())
                    .ok_or(ProvenanceFailure::UngroundedRoot)?;
                active.insert(id.clone());
                stack.push_back((Pointer::Generated(id), true));
                stack.extend(children.iter().cloned().map(|p| (p, false)));
                if stack.len() + visited.len() > MAX_GRAPH_NODES {
                    return Err(ProvenanceFailure::UngroundedRoot);
                }
            }
        }
        if leaves.len() > MAX_GRAPH_NODES {
            return Err(ProvenanceFailure::UngroundedRoot);
        }
    }
    if leaves.is_empty() {
        return Err(ProvenanceFailure::UngroundedRoot);
    }
    Ok(leaves)
}
