use std::path::Path;

use crate::graph::Graph;
use crate::provider::Providers;
use crate::state::{State, write_state};
use crate::types::OperationResult;

pub fn refresh(state: &mut State, state_path: &Path, providers: &Providers) -> Result<(), String> {
    let graph = Graph::from_state(state)?;
    let order = graph.topological_order();

    for node in order {
        let Some(name) = node.strip_prefix("resources.") else {
            continue;
        };
        let Some(res_state) = state.resources.get(name) else {
            continue;
        };

        let res_type = providers
            .resource_type(&res_state.resource_type)
            .ok_or_else(|| format!("Unknown resource type: {}", res_state.resource_type))?;

        match res_type.read(&res_state.outputs) {
            Ok(OperationResult::Success { outputs }) => {
                state.resources.get_mut(name).unwrap().outputs = outputs;
            }
            Ok(OperationResult::NotFound) => {
                eprintln!(
                    "  Warning: resource '{name}' not found at provider, removing from state"
                );
                state.resources.remove(name);
            }
            Ok(OperationResult::Failed { error, .. }) => {
                eprintln!("  Warning: failed to read '{name}': {error}, leaving state as-is");
            }
            Err(e) => {
                eprintln!("  Warning: error reading '{name}': {e}, leaving state as-is");
            }
        }
    }

    write_state(state_path, state)?;
    Ok(())
}

pub fn destroy(state: &mut State, state_path: &Path, providers: &Providers) -> Result<(), String> {
    let graph = Graph::from_state(state)?;
    let order = graph.reverse_topological_order();

    // DeployCtx not needed — destroy doesn't use ctx.save()
    let noop_ctx = NoopCtx;

    for node in order {
        let Some(name) = node.strip_prefix("resources.") else {
            continue;
        };
        let Some(res_state) = state.resources.get(name) else {
            continue;
        };

        let res_type = providers
            .resource_type(&res_state.resource_type)
            .ok_or_else(|| format!("Unknown resource type: {}", res_state.resource_type))?;

        match res_type.delete(&noop_ctx, &res_state.outputs) {
            Ok(OperationResult::Success { .. }) | Ok(OperationResult::NotFound) => {
                state.resources.remove(name);
                write_state(state_path, state)?;
            }
            Ok(OperationResult::Failed { error, .. }) => {
                write_state(state_path, state)?;
                return Err(format!("Failed to delete '{name}': {error}"));
            }
            Err(e) => {
                write_state(state_path, state)?;
                return Err(format!("Error deleting '{name}': {e}"));
            }
        }
    }

    write_state(state_path, state)?;
    Ok(())
}

struct NoopCtx;

impl crate::provider::OperationCtx for NoopCtx {
    fn save(&self, _outputs: &serde_json::Value) {
        // Destroy doesn't need intermediate saves
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::blue;
    use crate::state::ResourceState;
    use serde_json::json;
    use std::fs;

    fn setup() -> (Providers, String) {
        let mut providers = Providers::new();
        blue::register(&mut providers, None);
        let path = format!("/tmp/blue_test_refresh_{}.json", uuid::Uuid::new_v4());
        (providers, path)
    }

    #[test]
    fn refresh_updates_outputs() {
        let (providers, path) = setup();
        let mut state = State::new();
        state.resources.insert(
            "test".to_string(),
            ResourceState {
                resource_type: "blue.script".to_string(),
                inputs: json!({"script": "test.js"}),
                outputs: json!({"result": "old"}),
                depends_on: vec![],
            },
        );

        refresh(&mut state, Path::new(&path), &providers).unwrap();
        // Script resource read returns stored outputs unchanged
        assert_eq!(state.resources["test"].outputs["result"], "old");

        fs::remove_file(&path).ok();
    }

    #[test]
    fn destroy_removes_all() {
        let (providers, path) = setup();
        let mut state = State::new();
        state.resources.insert(
            "a".to_string(),
            ResourceState {
                resource_type: "blue.script".to_string(),
                inputs: json!({}),
                outputs: json!({}),
                depends_on: vec![],
            },
        );
        state.resources.insert(
            "b".to_string(),
            ResourceState {
                resource_type: "blue.script".to_string(),
                inputs: json!({}),
                outputs: json!({}),
                depends_on: vec!["resources.a".to_string()],
            },
        );

        destroy(&mut state, Path::new(&path), &providers).unwrap();
        assert!(state.resources.is_empty());

        fs::remove_file(&path).ok();
    }
}
