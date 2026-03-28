use std::collections::HashMap;

use petgraph::algo::toposort;
use petgraph::graph::{DiGraph, NodeIndex};

use crate::config;
use crate::reference::Ref;

#[derive(Debug, Clone, PartialEq)]
pub enum NodeKind {
    Data,
    Resource,
}

pub struct DependencyGraph {
    graph: DiGraph<(String, NodeKind), ()>,
    node_indices: HashMap<String, NodeIndex>,
}

impl DependencyGraph {
    /// Build a unified graph from data sources and resources.
    /// Edges are derived from `{{...}}` refs in properties and filters.
    pub fn build(
        data_sources: &HashMap<String, config::DataSource>,
        resources: &HashMap<String, config::Resource>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut graph = DiGraph::new();
        let mut node_indices = HashMap::new();

        // Add data sources as nodes
        for name in data_sources.keys() {
            let idx = graph.add_node((name.clone(), NodeKind::Data));
            node_indices.insert(name.clone(), idx);
        }

        // Add resources as nodes
        for name in resources.keys() {
            let idx = graph.add_node((name.clone(), NodeKind::Resource));
            node_indices.insert(name.clone(), idx);
        }

        // Add edges from resource property refs
        for (name, resource) in resources {
            if let Some(props) = &resource.properties {
                let text = props.to_string();
                for r in Ref::parse_all(&text) {
                    let dep_name = r.target();
                    match node_indices.get(dep_name) {
                        Some(&from) => {
                            let to = node_indices[name];
                            if from != to {
                                graph.add_edge(from, to, ());
                            }
                        }
                        None => {
                            return Err(format!(
                                "resource '{name}' references unknown node '{dep_name}'"
                            )
                            .into());
                        }
                    }
                }
            }
        }

        Ok(Self {
            graph,
            node_indices,
        })
    }

    /// Build from resource snapshots only (used by destroy and deploy-from-plan).
    pub fn build_from_snapshots(
        snapshots: &HashMap<String, crate::state::ResourceSnapshot>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut graph = DiGraph::new();
        let mut node_indices = HashMap::new();

        for name in snapshots.keys() {
            let idx = graph.add_node((name.clone(), NodeKind::Resource));
            node_indices.insert(name.clone(), idx);
        }

        for (name, snap) in snapshots {
            let text = snap.properties.to_string();
            for r in Ref::parse_all(&text) {
                let dep_name = r.target().to_string();
                if let Some(&from) = node_indices.get(&dep_name) {
                    let to = node_indices[name];
                    if from != to {
                        graph.add_edge(from, to, ());
                    }
                }
                // Skip refs to unknown nodes (data sources not in snapshots)
            }
        }

        Ok(Self {
            graph,
            node_indices,
        })
    }

    /// Topological sort returning (name, kind) pairs.
    pub fn topological_sort(&self) -> Result<Vec<(String, NodeKind)>, Box<dyn std::error::Error>> {
        let sorted = toposort(&self.graph, None).map_err(|cycle| {
            let (node_name, _) = &self.graph[cycle.node_id()];
            format!("dependency cycle detected involving '{node_name}'")
        })?;

        Ok(sorted
            .into_iter()
            .map(|idx| self.graph[idx].clone())
            .collect())
    }

    /// Topological sort returning just names (backward compat).
    pub fn topological_sort_names(&self) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        Ok(self
            .topological_sort()?
            .into_iter()
            .map(|(name, _)| name)
            .collect())
    }
}
