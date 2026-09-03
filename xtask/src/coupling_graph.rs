//! Deterministic graph algorithms for the production coupling gate.

use crate::coupling::{Edge, Graph};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn adjacency(graph: &Graph) -> BTreeMap<String, BTreeSet<String>> {
    let mut result: BTreeMap<_, BTreeSet<_>> = graph
        .modules
        .iter()
        .map(|module| (module.clone(), BTreeSet::new()))
        .collect();
    for edge in &graph.edges {
        result
            .get_mut(&edge.from)
            .expect("edge source is a module")
            .insert(edge.to.clone());
    }
    result
}

pub(super) fn strongly_connected_components(graph: &Graph) -> Vec<Vec<String>> {
    let adjacent = adjacency(graph);
    let mut reverse: BTreeMap<String, BTreeSet<String>> = graph
        .modules
        .iter()
        .map(|module| (module.clone(), BTreeSet::new()))
        .collect();
    for edge in &graph.edges {
        reverse
            .get_mut(&edge.to)
            .expect("edge target is a module")
            .insert(edge.from.clone());
    }
    let mut visited = BTreeSet::new();
    let mut order = Vec::new();
    for module in &graph.modules {
        visit(module, &adjacent, &mut visited, &mut order);
    }
    visited.clear();
    let mut result = Vec::new();
    for module in order.into_iter().rev() {
        if !visited.contains(&module) {
            let mut component = Vec::new();
            collect_component(&module, &reverse, &mut visited, &mut component);
            component.sort();
            result.push(component);
        }
    }
    result.sort();
    result
}

fn visit(
    module: &str,
    graph: &BTreeMap<String, BTreeSet<String>>,
    visited: &mut BTreeSet<String>,
    order: &mut Vec<String>,
) {
    if !visited.insert(module.to_owned()) {
        return;
    }
    for target in &graph[module] {
        visit(target, graph, visited, order);
    }
    order.push(module.to_owned());
}

fn collect_component(
    module: &str,
    graph: &BTreeMap<String, BTreeSet<String>>,
    visited: &mut BTreeSet<String>,
    component: &mut Vec<String>,
) {
    if !visited.insert(module.to_owned()) {
        return;
    }
    component.push(module.to_owned());
    for target in &graph[module] {
        collect_component(target, graph, visited, component);
    }
}

pub(super) fn cycle_forming_edges(graph: &Graph) -> BTreeSet<Edge> {
    let mut component_by_module = BTreeMap::new();
    for (index, component) in strongly_connected_components(graph).iter().enumerate() {
        for module in component {
            component_by_module.insert(module.clone(), (index, component.len()));
        }
    }
    graph
        .edges
        .iter()
        .filter(|edge| {
            let from = component_by_module[&edge.from];
            let to = component_by_module[&edge.to];
            from.0 == to.0 && (from.1 > 1 || edge.from == edge.to)
        })
        .cloned()
        .collect()
}
