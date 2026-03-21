use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use crate::config;

pub struct DependencyGraph {
    edges: BTreeMap<String, BTreeSet<String>>, // resource -> depends on
}

impl DependencyGraph {
    pub fn build_from_snapshots(
        snapshots: &HashMap<String, crate::state::ResourceSnapshot>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

        for (name, snap) in snapshots {
            edges.entry(name.clone()).or_default();
            let text = snap.properties.to_string();
            let refs = config::extract_resource_refs(&text);
            for (dep_name, _field) in refs {
                if !snapshots.contains_key(&dep_name) {
                    return Err(format!(
                        "resource '{name}' references unknown resource '{dep_name}'"
                    )
                    .into());
                }
                edges.entry(name.clone()).or_default().insert(dep_name);
            }
        }

        Ok(Self { edges })
    }

    pub fn topological_sort(&self) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        // Kahn's algorithm
        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        for node in self.edges.keys() {
            in_degree.entry(node).or_insert(0);
        }
        for deps in self.edges.values() {
            for dep in deps {
                *in_degree.entry(dep).or_insert(0) += 1;
            }
        }

        // Note: in_degree counts how many nodes depend ON this node.
        // Wait — Kahn's needs the reverse: in_degree = number of dependencies a node has.
        // Let me redo this. edges[A] = {B} means A depends on B. So B must come before A.
        // In a standard DAG for Kahn's: in_degree[A] = number of nodes A depends on = edges[A].len()

        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        for (node, deps) in &self.edges {
            in_degree.entry(node.as_str()).or_insert(0);
            for dep in deps {
                in_degree.entry(dep.as_str()).or_insert(0);
            }
        }
        for (node, deps) in &self.edges {
            *in_degree.get_mut(node.as_str()).unwrap() = deps.len();
        }

        let mut queue: VecDeque<String> = VecDeque::new();
        for (node, &deg) in &in_degree {
            if deg == 0 {
                queue.push_back(node.to_string());
            }
        }

        // Sort queue for deterministic ordering
        let mut sorted_queue: Vec<String> = queue.into_iter().collect();
        sorted_queue.sort();
        queue = sorted_queue.into_iter().collect();

        let mut order = Vec::new();
        while let Some(node) = queue.pop_front() {
            order.push(node.clone());
            // Find all nodes that depend on this node and reduce their in_degree
            for (dependent, deps) in &self.edges {
                if deps.contains(&node) {
                    let deg = in_degree.get_mut(dependent.as_str()).unwrap();
                    *deg -= 1;
                    if *deg == 0 {
                        // Insert in sorted position to maintain deterministic ordering
                        let insert_pos = queue
                            .iter()
                            .position(|x| x > dependent)
                            .unwrap_or(queue.len());
                        queue.insert(insert_pos, dependent.clone());
                    }
                }
            }
        }

        if order.len() != in_degree.len() {
            let remaining: Vec<&str> = in_degree
                .iter()
                .filter(|(_, deg)| **deg > 0)
                .map(|(&node, _)| node)
                .collect();
            return Err(format!(
                "dependency cycle detected among resources: {}",
                remaining.join(", ")
            )
            .into());
        }

        Ok(order)
    }
}
