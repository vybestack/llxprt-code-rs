//! Deterministic graph algorithms for the production coupling gate.

use crate::coupling::{Edge, Graph};
use std::collections::{BTreeMap, BTreeSet};

/// Largest SCC for which the gate uses the exponential exact algorithm.
///
/// The current production graph is comfortably below this bound. Larger SCCs use the explicit,
/// deterministic fallback below so graph growth cannot make the release gate unusable.
const EXACT_SCC_LIMIT: usize = 20;

#[derive(Debug, Default, Eq, PartialEq)]
pub(super) struct FeedbackArcSet {
    pub edges: BTreeSet<Edge>,
    pub fallback_components: Vec<Vec<String>>,
}

pub(super) fn adjacency(graph: &Graph) -> BTreeMap<String, BTreeSet<String>> {
    let mut result: BTreeMap<_, BTreeSet<_>> = graph
        .modules
        .iter()
        .cloned()
        .map(|module| (module, BTreeSet::new()))
        .collect();
    for edge in &graph.edges {
        result
            .get_mut(&edge.from)
            .expect("edge source belongs to graph")
            .insert(edge.to.clone());
    }
    result
}

pub(super) fn strongly_connected_components(graph: &Graph) -> Vec<Vec<String>> {
    fn visit(
        node: &str,
        graph: &BTreeMap<String, BTreeSet<String>>,
        visited: &mut BTreeSet<String>,
        order: &mut Vec<String>,
    ) {
        if !visited.insert(node.to_owned()) {
            return;
        }
        for next in &graph[node] {
            visit(next, graph, visited, order);
        }
        order.push(node.to_owned());
    }

    let forward = adjacency(graph);
    let mut reverse: BTreeMap<String, BTreeSet<String>> = graph
        .modules
        .iter()
        .cloned()
        .map(|module| (module, BTreeSet::new()))
        .collect();
    for edge in &graph.edges {
        reverse
            .get_mut(&edge.to)
            .expect("edge target belongs to graph")
            .insert(edge.from.clone());
    }

    let mut visited = BTreeSet::new();
    let mut order = Vec::new();
    for module in &graph.modules {
        visit(module, &forward, &mut visited, &mut order);
    }

    let mut assigned = BTreeSet::new();
    let mut components = Vec::new();
    while let Some(root) = order.pop() {
        if assigned.contains(&root) {
            continue;
        }
        let mut stack = vec![root];
        let mut component = Vec::new();
        while let Some(node) = stack.pop() {
            if !assigned.insert(node.clone()) {
                continue;
            }
            component.push(node.clone());
            // Reverse iteration makes the traversal's result independent of hash/random order.
            stack.extend(reverse[&node].iter().rev().cloned());
        }
        component.sort();
        components.push(component);
    }
    components.sort();
    components
}

/// Compute one deterministic minimum feedback arc set per SCC.
///
/// For SCCs of at most [`EXACT_SCC_LIMIT`] vertices, dynamic programming finds an ordering with
/// the minimum number of backward edges. Those backward edges (plus unavoidable self-loops) are
/// a minimum feedback arc set. Ties are resolved by sorted vertex index. For unexpectedly larger
/// SCCs, the deterministic fallback uses a sorted greedy ordering and is surfaced to callers.
pub(super) fn minimum_feedback_arc_set(graph: &Graph) -> FeedbackArcSet {
    let mut result = FeedbackArcSet::default();
    for component in strongly_connected_components(graph) {
        let members: BTreeSet<_> = component.iter().cloned().collect();
        let internal: Vec<_> = graph
            .edges
            .iter()
            .filter(|edge| members.contains(&edge.from) && members.contains(&edge.to))
            .cloned()
            .collect();
        if internal.is_empty() {
            continue;
        }

        let order = if component.len() <= EXACT_SCC_LIMIT {
            exact_minimum_order(&component, &internal)
        } else {
            result.fallback_components.push(component.clone());
            greedy_order(&component, &internal)
        };
        let positions: BTreeMap<_, _> = order
            .iter()
            .enumerate()
            .map(|(position, module)| (module.as_str(), position))
            .collect();
        result.edges.extend(internal.into_iter().filter(|edge| {
            edge.from == edge.to || positions[edge.from.as_str()] > positions[edge.to.as_str()]
        }));
    }
    result
}

fn exact_minimum_order(component: &[String], edges: &[Edge]) -> Vec<String> {
    let size = 1usize << component.len();
    let indexes: BTreeMap<_, _> = component
        .iter()
        .enumerate()
        .map(|(index, module)| (module.as_str(), index))
        .collect();
    let mut outgoing = vec![0usize; component.len()];
    for edge in edges.iter().filter(|edge| edge.from != edge.to) {
        outgoing[indexes[edge.from.as_str()]] |= 1usize << indexes[edge.to.as_str()];
    }

    let mut costs = vec![usize::MAX; size];
    let mut previous = vec![usize::MAX; size];
    costs[0] = 0;
    for mask in 1..size {
        // Put `vertex` last. Every edge from it to an already placed vertex is backward.
        for (vertex, outgoing_edges) in outgoing.iter().enumerate() {
            let bit = 1usize << vertex;
            if mask & bit == 0 {
                continue;
            }
            let prior = mask ^ bit;
            let candidate = costs[prior] + (*outgoing_edges & prior).count_ones() as usize;
            if candidate < costs[mask] || (candidate == costs[mask] && vertex < previous[mask]) {
                costs[mask] = candidate;
                previous[mask] = vertex;
            }
        }
    }

    let mut reversed = Vec::with_capacity(component.len());
    let mut mask = size - 1;
    while mask != 0 {
        let vertex = previous[mask];
        reversed.push(component[vertex].clone());
        mask ^= 1usize << vertex;
    }
    reversed.reverse();
    reversed
}

fn greedy_order(component: &[String], edges: &[Edge]) -> Vec<String> {
    let mut remaining: BTreeSet<_> = component.iter().cloned().collect();
    let mut order = Vec::with_capacity(component.len());
    while !remaining.is_empty() {
        let vertex = remaining
            .iter()
            .max_by_key(|candidate| {
                let outgoing = edges
                    .iter()
                    .filter(|edge| &edge.from == *candidate && remaining.contains(edge.to.as_str()))
                    .count();
                let incoming = edges
                    .iter()
                    .filter(|edge| &edge.to == *candidate && remaining.contains(edge.from.as_str()))
                    .count();
                (
                    outgoing as isize - incoming as isize,
                    std::cmp::Reverse((*candidate).clone()),
                )
            })
            .expect("remaining is non-empty")
            .clone();
        remaining.remove(&vertex);
        order.push(vertex);
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(modules: &[&str], edges: &[(&str, &str)]) -> Graph {
        Graph {
            modules: modules.iter().map(|value| (*value).to_owned()).collect(),
            edges: edges
                .iter()
                .map(|(from, to)| Edge {
                    from: (*from).to_owned(),
                    to: (*to).to_owned(),
                })
                .collect(),
        }
    }

    #[test]
    fn feedback_set_is_minimum_not_every_edge_in_an_scc() {
        let graph = graph(
            &["a", "b", "c"],
            &[("a", "b"), ("b", "c"), ("c", "a"), ("a", "c")],
        );

        let feedback = minimum_feedback_arc_set(&graph);

        assert_eq!(feedback.edges.len(), 1);
        assert!(feedback.fallback_components.is_empty());
        assert_ne!(feedback.edges.len(), graph.edges.len());
        let retained = Graph {
            modules: graph.modules.clone(),
            edges: graph.edges.difference(&feedback.edges).cloned().collect(),
        };
        assert!(strongly_connected_components(&retained)
            .iter()
            .all(|component| component.len() == 1));
    }

    #[test]
    fn exact_search_beats_a_simple_source_order() {
        let graph = graph(
            &["a", "b", "c"],
            &[("b", "a"), ("c", "a"), ("c", "b"), ("a", "c")],
        );
        let feedback = minimum_feedback_arc_set(&graph);
        assert_eq!(feedback.edges.len(), 1);
    }
}
