use std::collections::HashMap;

use serde_json::Value;

use crate::config::ResourceConfig;
use crate::diff::diff_resource;
use crate::graph::Graph;
use crate::provider::Providers;
use crate::state::State;
use crate::template::resolve_value;
use crate::types::{Action, Diff};

#[derive(Debug)]
pub struct Plan {
    pub lineage: String,
    pub serial: u64,
    pub steps: Vec<PlanStep>,
}

#[derive(Debug)]
pub struct PlanStep {
    pub name: String,
    pub resource_type: String,
    pub action: Action,
    pub diff: Diff,
    pub resolved_inputs: Option<Value>,
    pub depends_on: Vec<String>,
}

pub fn create_plan(
    config: &ResourceConfig,
    state: &State,
    providers: &Providers,
    params: &HashMap<String, Value>,
) -> Result<Plan, String> {
    let graph = Graph::from_config_and_state(config, state)?;
    let order = graph.topological_order();
    let mut output_map: HashMap<String, Value> = HashMap::new();
    let mut diffs: HashMap<String, Diff> = HashMap::new();
    let mut resolved_inputs_map: HashMap<String, Value> = HashMap::new();

    for node in &order {
        if let Some(name) = node.strip_prefix("parameters.") {
            let value = resolve_parameter(name, config, params)?;
            output_map.insert(node.to_string(), value);
        } else if let Some(name) = node.strip_prefix("data.") {
            let ds_config = &config.data[name];
            let resolved_config = resolve_value(
                &Value::Object(ds_config.config.clone().into_iter().collect()),
                &output_map,
            )?;
            let ds_type = providers
                .data_source_type(&ds_config.data_type)
                .ok_or_else(|| format!("Unknown data source type: {}", ds_config.data_type))?;
            let outputs = ds_type.read(resolved_config)?;
            output_map.insert(node.to_string(), outputs);
        } else if let Some(name) = node.strip_prefix("resources.") {
            // Add current state outputs to map (for downstream refs)
            if let Some(res_state) = state.resources.get(name) {
                output_map.insert(node.to_string(), res_state.outputs.clone());
            }

            // Resolve inputs if resource is in config
            if let Some(res_def) = config.resources.get(name) {
                let res_type = providers
                    .resource_type(&res_def.resource_type)
                    .ok_or_else(|| format!("Unknown resource type: {}", res_def.resource_type))?;
                let schema = res_type.schema();

                let resolved = match &res_def.inputs {
                    Some(inputs) => resolve_value(
                        &Value::Object(inputs.clone().into_iter().collect()),
                        &output_map,
                    )?,
                    None => Value::Object(serde_json::Map::new()),
                };

                res_type.validate(&resolved)?;

                let old_inputs = state.resources.get(name).map(|r| &r.inputs);
                let mut diff = diff_resource(schema, old_inputs, Some(&resolved));
                let current_outputs = state.resources.get(name)
                    .map(|r| &r.outputs)
                    .cloned()
                    .unwrap_or_default();
                res_type.customize_diff(&mut diff, &resolved, &current_outputs)?;
                diffs.insert(node.to_string(), diff);
                resolved_inputs_map.insert(node.to_string(), resolved);
            } else {
                // Deletion node — in state but not in config
                let res_state = &state.resources[name];
                let schema = providers
                    .resource_type(&res_state.resource_type)
                    .ok_or_else(|| {
                        format!("Unknown resource type: {}", res_state.resource_type)
                    })?
                    .schema();
                let diff = diff_resource(schema, Some(&res_state.inputs), None);
                diffs.insert(node.to_string(), diff);
            }
        }
    }

    // Cascade: propagate replacements to downstream force_new dependents
    cascade_replacements(&order, config, providers, &mut diffs)?;

    // Build steps from diffs
    let mut steps = Vec::new();
    for node in &order {
        if let Some(name) = node.strip_prefix("resources.") {
            if let Some(diff) = diffs.remove(*node) {
                if diff.action == Action::Unchanged {
                    continue;
                }
                let resource_type = config
                    .resources
                    .get(name)
                    .map(|r| r.resource_type.clone())
                    .unwrap_or_else(|| state.resources[name].resource_type.clone());
                let depends_on = graph.resource_dependencies(node);
                steps.push(PlanStep {
                    name: name.to_string(),
                    resource_type,
                    action: diff.action.clone(),
                    diff,
                    resolved_inputs: resolved_inputs_map.remove(*node),
                    depends_on,
                });
            }
        }
    }

    Ok(Plan {
        lineage: state.lineage.clone(),
        serial: state.serial,
        steps,
    })
}

fn resolve_parameter(
    name: &str,
    config: &ResourceConfig,
    params: &HashMap<String, Value>,
) -> Result<Value, String> {
    // CLI --var overrides
    if let Some(val) = params.get(name) {
        return Ok(val.clone());
    }
    let param_config = config
        .parameters
        .get(name)
        .ok_or_else(|| format!("Parameter '{name}' not found in config"))?;
    // Env var
    if let Some(env_name) = &param_config.env {
        if let Ok(val) = std::env::var(env_name) {
            return Ok(Value::String(val));
        }
    }
    // Default
    if let Some(default) = &param_config.default {
        return Ok(default.clone());
    }
    Err(format!("No value for parameter '{name}'"))
}

fn cascade_replacements(
    order: &[&str],
    config: &ResourceConfig,
    providers: &Providers,
    diffs: &mut HashMap<String, Diff>,
) -> Result<(), String> {
    loop {
        let replaced: Vec<String> = diffs
            .iter()
            .filter(|(_, d)| d.action == Action::Replace)
            .map(|(name, _)| name.clone())
            .collect();

        if replaced.is_empty() {
            return Ok(());
        }

        let mut new_replacements = false;

        for node in order {
            let Some(name) = node.strip_prefix("resources.") else {
                continue;
            };
            let Some(res_def) = config.resources.get(name) else {
                continue;
            };
            let Some(diff) = diffs.get(*node) else {
                continue;
            };
            if diff.action == Action::Replace || diff.action == Action::Create || diff.action == Action::Delete {
                continue;
            }

            let Some(inputs) = &res_def.inputs else {
                continue;
            };

            let schema = providers
                .resource_type(&res_def.resource_type)
                .ok_or_else(|| format!("Unknown resource type: {}", res_def.resource_type))?
                .schema();

            let mut needs_replace = false;
            for (field_name, value) in inputs {
                let is_force_new = schema.inputs.iter().any(|f| f.path == *field_name && f.force_new);
                if is_force_new && refs_to_replaced(value, &replaced)? {
                    needs_replace = true;
                    break;
                }
            }

            if needs_replace {
                if let Some(diff) = diffs.get_mut(*node) {
                    diff.action = Action::Replace;
                    new_replacements = true;
                }
            }
        }

        if !new_replacements {
            return Ok(());
        }
    }
}

fn refs_to_replaced(value: &Value, replaced: &[String]) -> Result<bool, String> {
    match value {
        Value::String(s) => {
            for r in crate::template::extract_refs(s)? {
                if replaced.contains(&r.dependency_key()) {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Value::Object(map) => {
            for v in map.values() {
                if refs_to_replaced(v, replaced)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Value::Array(arr) => {
            for v in arr {
                if refs_to_replaced(v, replaced)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse_resource_config;
    use crate::providers::blue;
    use crate::state::{ResourceState, State};
    use serde_json::json;

    fn setup_providers() -> Providers {
        let mut providers = Providers::new();
        blue::register(&mut providers, None);
        providers
    }

    #[test]
    fn plan_create_new_resource() {
        let config = parse_resource_config(r#"
[resources.test]
type = "blue.script"

[resources.test.inputs]
script = "test.js"
triggers_replace = { key = "value" }
"#).unwrap();

        let state = State::new();
        let providers = setup_providers();
        let plan = create_plan(&config, &state, &providers, &HashMap::new()).unwrap();

        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].name, "test");
        assert_eq!(plan.steps[0].action, Action::Create);
    }

    #[test]
    fn plan_delete_removed_resource() {
        let config = parse_resource_config("").unwrap();

        let mut state = State::new();
        state.resources.insert("old".to_string(), ResourceState {
            resource_type: "blue.script".to_string(),
            inputs: json!({"script": "old.js"}),
            outputs: json!({}),
            depends_on: vec![],
        });

        let providers = setup_providers();
        let plan = create_plan(&config, &state, &providers, &HashMap::new()).unwrap();

        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].name, "old");
        assert_eq!(plan.steps[0].action, Action::Delete);
    }

    #[test]
    fn plan_unchanged_resource() {
        let config = parse_resource_config(r#"
[resources.test]
type = "blue.script"

[resources.test.inputs]
script = "test.js"
triggers_replace = { key = "value" }
"#).unwrap();

        let mut state = State::new();
        state.resources.insert("test".to_string(), ResourceState {
            resource_type: "blue.script".to_string(),
            inputs: json!({"script": "test.js", "triggers_replace": {"key": "value"}}),
            outputs: json!({}),
            depends_on: vec![],
        });

        let providers = setup_providers();
        let plan = create_plan(&config, &state, &providers, &HashMap::new()).unwrap();

        assert!(plan.steps.is_empty()); // unchanged resources don't produce steps
    }

    #[test]
    fn plan_replace_on_force_new_change() {
        let config = parse_resource_config(r#"
[resources.test]
type = "blue.script"

[resources.test.inputs]
script = "new_script.js"
triggers_replace = { key = "value" }
"#).unwrap();

        let mut state = State::new();
        state.resources.insert("test".to_string(), ResourceState {
            resource_type: "blue.script".to_string(),
            inputs: json!({"script": "old_script.js", "triggers_replace": {"key": "value"}}),
            outputs: json!({}),
            depends_on: vec![],
        });

        let providers = setup_providers();
        let plan = create_plan(&config, &state, &providers, &HashMap::new()).unwrap();

        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].action, Action::Replace);
    }

    #[test]
    fn plan_with_parameter() {
        let config = parse_resource_config(r#"
[parameters.name]
default = "test-server"

[resources.test]
type = "blue.script"

[resources.test.inputs]
script = "test.js"
triggers_replace = { name = "{{ parameters.name }}" }
"#).unwrap();

        let state = State::new();
        let providers = setup_providers();
        let plan = create_plan(&config, &state, &providers, &HashMap::new()).unwrap();

        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].action, Action::Create);
        let inputs = plan.steps[0].resolved_inputs.as_ref().unwrap();
        assert_eq!(inputs["triggers_replace"]["name"], "test-server");
    }
}
