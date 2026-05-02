use std::collections::HashMap;

use petgraph::algo::toposort;
use petgraph::graph::{DiGraph, NodeIndex};
use serde_json::Value;

use crate::config::ResourceConfig;
use crate::state::State;
use crate::template::extract_refs;

#[derive(Debug)]
pub struct Graph {
    inner: DiGraph<String, ()>,
    node_indices: HashMap<String, NodeIndex>,
    sorted: Vec<String>,
}

impl Graph {
    /// Build from config + state — used by plan.
    pub fn from_config_and_state(config: &ResourceConfig, state: &State) -> Result<Self, String> {
        let mut graph = Self::empty();

        // Add parameter nodes
        for name in config.parameters.keys() {
            graph.add_node(&format!("parameters.{name}"));
        }

        // Add data source nodes
        for name in config.data.keys() {
            graph.add_node(&format!("data.{name}"));
        }

        // Add resource nodes from config
        for name in config.resources.keys() {
            graph.add_node(&format!("resources.{name}"));
        }

        // Add deletion nodes — resources in state but not in config
        for name in state.resources.keys() {
            if !config.resources.contains_key(name) {
                graph.add_node(&format!("resources.{name}"));
            }
        }

        // Add edges from {{ }} refs in data source configs
        for (name, ds) in &config.data {
            let node_key = format!("data.{name}");
            for v in ds.config.values() {
                graph.add_edges_from_value(&node_key, v)?;
            }
        }

        // Add edges from {{ }} refs in resource inputs
        for (name, res) in &config.resources {
            let node_key = format!("resources.{name}");
            for v in res.config.values() {
                graph.add_edges_from_value(&node_key, v)?;
            }
        }

        // Add edges for deletion nodes from state's depends_on
        for (name, res_state) in &state.resources {
            if config.resources.contains_key(name) {
                continue; // config edges take priority
            }
            let node_key = format!("resources.{name}");
            for dep in &res_state.depends_on {
                if graph.node_indices.contains_key(dep) {
                    graph.add_edge(dep, &node_key)?;
                }
                // ignore edges to nodes that don't exist — dependency is already gone
            }
        }

        // Validate: data sources only depend on parameters and other data sources
        for name in config.data.keys() {
            let node_key = format!("data.{name}");
            let idx = graph.node_indices[&node_key];
            for neighbor in graph
                .inner
                .neighbors_directed(idx, petgraph::Direction::Incoming)
            {
                let dep = &graph.inner[neighbor];
                if dep.starts_with("resources.") {
                    return Err(format!(
                        "Data source '{name}' depends on resource '{dep}' — data sources may only depend on parameters and other data sources"
                    ));
                }
            }
        }

        // Validate: no cycles, cache sorted order
        graph.sorted = toposort(&graph.inner, None)
            .map_err(|e| {
                format!(
                    "Cycle detected involving node '{}'",
                    graph.inner[e.node_id()]
                )
            })?
            .iter()
            .map(|idx| graph.inner[*idx].clone())
            .collect();

        Ok(graph)
    }

    /// Build from state only — used by refresh and destroy.
    pub fn from_state(state: &State) -> Result<Self, String> {
        let mut graph = Self::empty();

        for name in state.resources.keys() {
            graph.add_node(&format!("resources.{name}"));
        }

        for (name, res_state) in &state.resources {
            let node_key = format!("resources.{name}");
            for dep in &res_state.depends_on {
                if graph.node_indices.contains_key(dep) {
                    graph.add_edge(dep, &node_key)?;
                }
            }
        }

        graph.sorted = toposort(&graph.inner, None)
            .map_err(|e| {
                format!(
                    "Cycle detected involving node '{}'",
                    graph.inner[e.node_id()]
                )
            })?
            .iter()
            .map(|idx| graph.inner[*idx].clone())
            .collect();

        Ok(graph)
    }

    /// Forward topological order.
    pub fn topological_order(&self) -> Vec<&str> {
        self.sorted.iter().map(|s| s.as_str()).collect()
    }

    /// Get resource dependencies for a node (only "resources.*" deps).
    pub fn resource_dependencies(&self, node: &str) -> Vec<String> {
        let Some(&idx) = self.node_indices.get(node) else {
            return vec![];
        };
        self.inner
            .neighbors_directed(idx, petgraph::Direction::Incoming)
            .filter_map(|dep_idx| {
                let dep = &self.inner[dep_idx];
                if dep.starts_with("resources.") {
                    Some(dep.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Reverse topological order — used by destroy.
    pub fn reverse_topological_order(&self) -> Vec<&str> {
        let mut order = self.topological_order();
        order.reverse();
        order
    }

    fn empty() -> Self {
        Graph {
            inner: DiGraph::new(),
            node_indices: HashMap::new(),
            sorted: Vec::new(),
        }
    }

    fn add_node(&mut self, key: &str) {
        let idx = self.inner.add_node(key.to_string());
        self.node_indices.insert(key.to_string(), idx);
    }

    fn add_edge(&mut self, from: &str, to: &str) -> Result<(), String> {
        let from_idx = self
            .node_indices
            .get(from)
            .ok_or_else(|| format!("Ref target '{from}' not found in graph"))?;
        let to_idx = self
            .node_indices
            .get(to)
            .ok_or_else(|| format!("Node '{to}' not found in graph"))?;
        self.inner.add_edge(*from_idx, *to_idx, ());
        Ok(())
    }

    fn add_edges_from_value(&mut self, node_key: &str, value: &Value) -> Result<(), String> {
        match value {
            Value::String(s) => {
                for r in extract_refs(s)? {
                    let dep_key = r.dependency_key();
                    if !self.node_indices.contains_key(&dep_key) {
                        return Err(format!(
                            "Ref '{{{{ {dep_key} }}}}' in '{node_key}' points to unknown node"
                        ));
                    }
                    self.add_edge(&dep_key, node_key)?;
                }
            }
            Value::Object(map) => {
                for v in map.values() {
                    self.add_edges_from_value(node_key, v)?;
                }
            }
            Value::Array(arr) => {
                for v in arr {
                    self.add_edges_from_value(node_key, v)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse_resource_config;
    use crate::state::{ResourceState, State};
    use serde_json::json;

    fn empty_state() -> State {
        State::new()
    }

    #[test]
    fn basic_graph_from_config() {
        let config = parse_resource_config(
            r#"
[parameters.region]
default = "uk-lon1"

[data.ubuntu]
type = "upcloud.storage"
filters = { title = "Ubuntu Server 24.04 LTS" }

[resources.web-01]
type = "upcloud.server"
zone = "{{ parameters.region }}"
storage = "{{ data.ubuntu.uuid }}"
"#,
        )
        .unwrap();

        let graph = Graph::from_config_and_state(&config, &empty_state()).unwrap();
        let order = graph.topological_order();

        // parameters and data must come before resources
        let param_pos = order
            .iter()
            .position(|n| *n == "parameters.region")
            .unwrap();
        let data_pos = order.iter().position(|n| *n == "data.ubuntu").unwrap();
        let res_pos = order.iter().position(|n| *n == "resources.web-01").unwrap();
        assert!(param_pos < res_pos);
        assert!(data_pos < res_pos);
    }

    #[test]
    fn deletion_node_added() {
        let config = parse_resource_config(
            r#"
[resources.web-01]
type = "upcloud.server"
hostname = "web-01"
"#,
        )
        .unwrap();

        let mut state = empty_state();
        state.resources.insert(
            "old-server".to_string(),
            ResourceState {
                resource_type: "upcloud.server".to_string(),
                inputs: json!({}),
                outputs: json!({}),
                depends_on: vec![],
            },
        );

        let graph = Graph::from_config_and_state(&config, &state).unwrap();
        let order = graph.topological_order();
        assert!(order.contains(&"resources.old-server"));
    }

    #[test]
    fn data_source_depends_on_resource_fails() {
        let config = parse_resource_config(
            r#"
[resources.web-01]
type = "upcloud.server"
hostname = "web-01"

[data.lookup]
type = "upcloud.storage"
filters = { uuid = "{{ resources.web-01.uuid }}" }
"#,
        )
        .unwrap();

        let result = Graph::from_config_and_state(&config, &empty_state());
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("data sources may only depend on parameters")
        );
    }

    #[test]
    fn cycle_detection() {
        let config = parse_resource_config(
            r#"
[resources.a]
type = "upcloud.server"
dep = "{{ resources.b.uuid }}"

[resources.b]
type = "upcloud.server"
dep = "{{ resources.a.uuid }}"
"#,
        )
        .unwrap();

        let result = Graph::from_config_and_state(&config, &empty_state());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Cycle detected"));
    }

    #[test]
    fn state_only_graph() {
        let mut state = empty_state();
        state.resources.insert(
            "server".to_string(),
            ResourceState {
                resource_type: "upcloud.server".to_string(),
                inputs: json!({}),
                outputs: json!({"uuid": "abc"}),
                depends_on: vec![],
            },
        );
        state.resources.insert(
            "firewall".to_string(),
            ResourceState {
                resource_type: "upcloud.firewall".to_string(),
                inputs: json!({}),
                outputs: json!({}),
                depends_on: vec!["resources.server".to_string()],
            },
        );

        let graph = Graph::from_state(&state).unwrap();
        let order = graph.topological_order();
        let server_pos = order.iter().position(|n| *n == "resources.server").unwrap();
        let fw_pos = order
            .iter()
            .position(|n| *n == "resources.firewall")
            .unwrap();
        assert!(server_pos < fw_pos);
    }

    #[test]
    fn reverse_order_for_destroy() {
        let mut state = empty_state();
        state.resources.insert(
            "server".to_string(),
            ResourceState {
                resource_type: "upcloud.server".to_string(),
                inputs: json!({}),
                outputs: json!({}),
                depends_on: vec![],
            },
        );
        state.resources.insert(
            "firewall".to_string(),
            ResourceState {
                resource_type: "upcloud.firewall".to_string(),
                inputs: json!({}),
                outputs: json!({}),
                depends_on: vec!["resources.server".to_string()],
            },
        );

        let graph = Graph::from_state(&state).unwrap();
        let order = graph.reverse_topological_order();
        let server_pos = order.iter().position(|n| *n == "resources.server").unwrap();
        let fw_pos = order
            .iter()
            .position(|n| *n == "resources.firewall")
            .unwrap();
        assert!(fw_pos < server_pos); // firewall deleted before server
    }

    #[test]
    fn unknown_ref_fails() {
        let config = parse_resource_config(
            r#"
[resources.web-01]
type = "upcloud.server"
storage = "{{ data.nonexistent.uuid }}"
"#,
        )
        .unwrap();

        let result = Graph::from_config_and_state(&config, &empty_state());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown node"));
    }
}
