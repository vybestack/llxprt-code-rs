//! Deterministic graph algorithms for the production coupling gate.

use crate::coupling::{Edge, Graph};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Default, Eq, PartialEq)]
pub(super) struct FeedbackArcSet {
    pub edges: BTreeSet<Edge>,
}

pub(super) fn adjacency(graph: &Graph) -> BTreeMap<String, Vec<String>> {
    let mut adjacency: BTreeMap<String, Vec<String>> = graph
        .modules
        .iter()
        .map(|node| (node.clone(), Vec::new()))
        .collect();
    for edge in &graph.edges {
        adjacency
            .entry(edge.from.clone())
            .or_default()
            .push(edge.to.clone());
    }
    for targets in adjacency.values_mut() {
        targets.sort();
        targets.dedup();
    }
    adjacency
}

pub(super) fn strongly_connected_components(graph: &Graph) -> Vec<Vec<String>> {
    fn visit(
        node: &str,
        adjacency: &BTreeMap<String, Vec<String>>,
        seen: &mut BTreeSet<String>,
        order: &mut Vec<String>,
    ) {
        if !seen.insert(node.to_owned()) {
            return;
        }
        if let Some(targets) = adjacency.get(node) {
            for target in targets {
                visit(target, adjacency, seen, order);
            }
        }
        order.push(node.to_owned());
    }
    let forward = adjacency(graph);
    let mut reverse: BTreeMap<String, Vec<String>> = graph
        .modules
        .iter()
        .map(|node| (node.clone(), Vec::new()))
        .collect();
    for edge in &graph.edges {
        reverse
            .entry(edge.to.clone())
            .or_default()
            .push(edge.from.clone());
    }
    for targets in reverse.values_mut() {
        targets.sort();
        targets.dedup();
    }
    let mut seen = BTreeSet::new();
    let mut order = Vec::new();
    for node in &graph.modules {
        visit(node, &forward, &mut seen, &mut order);
    }
    seen.clear();
    let mut components = Vec::new();
    for node in order.into_iter().rev() {
        if seen.contains(&node) {
            continue;
        }
        let mut component_order = Vec::new();
        visit(&node, &reverse, &mut seen, &mut component_order);
        component_order.sort();
        components.push(component_order);
    }
    components.sort();
    components
}

/// Computes a deterministic, exact minimum feedback arc set.
///
/// SCCs are independent. Within each SCC, backward edges of an ordering form a feedback set, and
/// every minimum feedback set is represented by an ordering. Branch-and-bound avoids allocating
/// a `2^n` table while retaining exactness for every graph size accepted by available resources.
pub(super) fn minimum_feedback_arc_set(graph: &Graph) -> FeedbackArcSet {
    let mut result = FeedbackArcSet::default();
    for component in strongly_connected_components(graph) {
        let members: BTreeSet<&str> = component.iter().map(String::as_str).collect();
        let internal: Vec<Edge> = graph
            .edges
            .iter()
            .filter(|edge| {
                members.contains(edge.from.as_str()) && members.contains(edge.to.as_str())
            })
            .cloned()
            .collect();
        if component.len() == 1 {
            result
                .edges
                .extend(internal.into_iter().filter(|edge| edge.from == edge.to));
            continue;
        }
        let order = exact_minimum_order(&component, &internal);
        let position: BTreeMap<&str, usize> = order
            .iter()
            .enumerate()
            .map(|(place, node)| (node.as_str(), place))
            .collect();
        result.edges.extend(
            internal
                .into_iter()
                .filter(|edge| position[edge.from.as_str()] >= position[edge.to.as_str()]),
        );
    }
    result
}

fn exact_minimum_order(component: &[String], edges: &[Edge]) -> Vec<String> {
    let index: BTreeMap<&str, usize> = component
        .iter()
        .enumerate()
        .map(|(index, node)| (node.as_str(), index))
        .collect();
    let edges: Vec<(usize, usize)> = edges
        .iter()
        .map(|edge| (index[edge.from.as_str()], index[edge.to.as_str()]))
        .collect();
    let mut best: Vec<usize> = (0..component.len()).collect();
    let mut best_cost = backward_count(&best, &edges);
    search_orders(
        &edges,
        &mut vec![false; component.len()],
        &mut Vec::new(),
        0,
        &mut best_cost,
        &mut best,
    );
    best.into_iter()
        .map(|index| component[index].clone())
        .collect()
}

fn backward_count(order: &[usize], edges: &[(usize, usize)]) -> usize {
    let mut position = vec![0; order.len()];
    for (place, vertex) in order.iter().copied().enumerate() {
        position[vertex] = place;
    }
    edges
        .iter()
        .filter(|(source, target)| position[*source] >= position[*target])
        .count()
}

fn search_orders(
    edges: &[(usize, usize)],
    placed: &mut [bool],
    prefix: &mut Vec<usize>,
    charged: usize,
    best_cost: &mut usize,
    best: &mut Vec<usize>,
) {
    let crossing = edges
        .iter()
        .filter(|(source, target)| !placed[*source] && placed[*target])
        .count();
    let topological = remaining_topological_order(edges, placed);
    if charged + crossing + usize::from(topological.is_none()) >= *best_cost {
        return;
    }
    if let Some(suffix) = topological {
        let mut candidate = prefix.clone();
        candidate.extend(suffix);
        *best_cost = charged + crossing;
        *best = candidate;
        return;
    }
    for vertex in 0..placed.len() {
        if placed[vertex] {
            continue;
        }
        let added = edges
            .iter()
            .filter(|(source, target)| *source == vertex && placed[*target])
            .count();
        if charged + added >= *best_cost {
            continue;
        }
        placed[vertex] = true;
        prefix.push(vertex);
        search_orders(edges, placed, prefix, charged + added, best_cost, best);
        prefix.pop();
        placed[vertex] = false;
    }
}

fn remaining_topological_order(edges: &[(usize, usize)], placed: &[bool]) -> Option<Vec<usize>> {
    let mut indegree = vec![0_usize; placed.len()];
    for &(source, target) in edges {
        if !placed[source] && !placed[target] {
            indegree[target] += 1;
        }
    }
    let mut ready: BTreeSet<usize> = (0..placed.len())
        .filter(|vertex| !placed[*vertex] && indegree[*vertex] == 0)
        .collect();
    let remaining = placed.iter().filter(|value| !**value).count();
    let mut order = Vec::with_capacity(remaining);
    while let Some(vertex) = ready.pop_first() {
        order.push(vertex);
        for &(source, target) in edges {
            if source == vertex && !placed[target] {
                indegree[target] -= 1;
                if indegree[target] == 0 {
                    ready.insert(target);
                }
            }
        }
    }
    (order.len() == remaining).then_some(order)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn graph(edges: &[(&str, &str)]) -> Graph {
        let mut graph = Graph {
            modules: BTreeSet::new(),
            edges: BTreeSet::new(),
        };
        for (source, target) in edges {
            graph.modules.insert((*source).into());
            graph.modules.insert((*target).into());
            graph.edges.insert(Edge {
                from: (*source).into(),
                to: (*target).into(),
            });
        }
        graph
    }

    #[test]
    fn returns_a_minimum_set_not_every_edge_in_an_scc() {
        let graph = graph(&[("a", "b"), ("b", "c"), ("c", "a"), ("a", "c")]);
        let feedback = minimum_feedback_arc_set(&graph);
        assert_eq!(feedback.edges.len(), 1);
        assert_eq!(
            feedback.edges,
            BTreeSet::from([Edge {
                from: "c".into(),
                to: "a".into()
            }])
        );
    }

    #[test]
    fn exact_search_beats_a_simple_source_order() {
        let graph = graph(&[("a", "b"), ("b", "a"), ("b", "c"), ("c", "a")]);
        let feedback = minimum_feedback_arc_set(&graph);
        assert_eq!(feedback.edges.len(), 1);
        assert_eq!(
            feedback.edges,
            BTreeSet::from([Edge {
                from: "a".into(),
                to: "b".into()
            }])
        );
    }

    #[test]
    fn exact_and_deterministic_above_twenty_vertices() {
        let mut edges = Vec::new();
        for index in 0..24 {
            edges.push((format!("n{index:02}"), format!("n{:02}", (index + 1) % 24)));
        }
        let borrowed: Vec<(&str, &str)> = edges
            .iter()
            .map(|(source, target)| (source.as_str(), target.as_str()))
            .collect();
        let graph = graph(&borrowed);
        let expected = BTreeSet::from([Edge {
            from: "n23".into(),
            to: "n00".into(),
        }]);
        for _ in 0..4 {
            assert_eq!(minimum_feedback_arc_set(&graph).edges, expected);
        }
    }
}
