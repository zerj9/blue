use std::collections::HashMap;

use serde_json::Value;

use crate::config::ResourceConfig;
use crate::diff::diff_resource;
use crate::graph::Graph;
use crate::provider::Providers;
use crate::resolvable::{Resolvable, resolve_inputs};
use crate::state::State;
use crate::types::{Action, Diff};

#[derive(Debug)]
pub struct Plan {
    pub lineage: String,
    pub serial: u64,
    pub steps: Vec<PlanStep>,
    /// Plan-time-known outputs: parameters, data source results, and
    /// outputs from resources that will be `Unchanged` in this plan. Seeds
    /// the deploy-time `output_map`, which then accumulates outputs from
    /// completed steps. Resources whose diff is Create/Update/Replace/
    /// Delete are deliberately excluded — their outputs are unstable
    /// across the plan and downstream resolution must wait for deploy.
    pub known_outputs: HashMap<String, Value>,
}

#[derive(Debug)]
pub struct PlanStep {
    pub name: String,
    pub resource_type: String,
    pub action: Action,
    pub diff: Diff,
    /// Resolved inputs as of plan time. May contain `Unknown` leaves for
    /// refs to resources being created/updated in this plan; deploy-time
    /// re-resolution against the accumulated `output_map` finalises these
    /// to concrete values before the provider is called.
    pub resolved_inputs: Option<Resolvable>,
    pub depends_on: Vec<String>,
}

pub fn create_plan(
    config: &ResourceConfig,
    state: &State,
    providers: &Providers,
    params: &HashMap<String, Value>,
) -> Result<Plan, String> {
    // Refuse to plan if any resource type in the config has secret outputs
    // but [encryption] recipients are absent. Without recipients, those
    // values would land in state cleartext — fail before any work happens.
    refuse_if_secrets_without_recipients(config, providers)?;

    let graph = Graph::from_config_and_state(config, state)?;
    let order = graph.topological_order();
    let mut output_map: HashMap<String, Value> = HashMap::new();
    let mut diffs: HashMap<String, Diff> = HashMap::new();
    let mut resolved_inputs_map: HashMap<String, Resolvable> = HashMap::new();

    for node in &order {
        if let Some(name) = node.strip_prefix("parameters.") {
            let value = resolve_parameter(name, config, params)?;
            output_map.insert(node.to_string(), value);
        } else if let Some(name) = node.strip_prefix("data.") {
            let ds_config = &config.data[name];
            let ds_type = providers
                .data_source_type(&ds_config.data_type)
                .ok_or_else(|| format!("Unknown data source type: {}", ds_config.data_type))?;
            let schema = ds_type.schema();
            let raw_config = Value::Object(ds_config.config.clone().into_iter().collect());
            let with_defaults = crate::schema::apply_defaults(&schema.inputs, raw_config);
            // Data sources can only depend on parameters and other data
            // sources (graph rule), so by construction every ref is
            // resolvable here. Strict-finalize via `into_concrete`; any
            // leftover Unknown indicates a graph bug.
            let resolved = resolve_inputs(&with_defaults, &schema.inputs, &output_map)?;
            let concrete = resolved.into_concrete().map_err(|errs| {
                let parts: Vec<String> = errs
                    .iter()
                    .map(|(p, r)| format!("{p} ({{{{ {} }}}})", r.dependency_key()))
                    .collect();
                format!(
                    "data source '{name}' has unresolved refs at plan time (this is a planner bug): {}",
                    parts.join(", ")
                )
            })?;
            crate::schema::validate_inputs(&schema.inputs, &concrete)
                .map_err(|e| format!("data source '{name}': {e}"))?;
            let outputs = ds_type.read(concrete)?;
            output_map.insert(node.to_string(), outputs);
        } else if let Some(name) = node.strip_prefix("resources.") {
            if let Some(res_def) = config.resources.get(name) {
                let res_type = providers
                    .resource_type(&res_def.resource_type)
                    .ok_or_else(|| format!("Unknown resource type: {}", res_def.resource_type))?;
                let schema = res_type.schema();

                let raw_config = Value::Object(res_def.config.clone().into_iter().collect());
                let with_defaults = crate::schema::apply_defaults(&schema.inputs, raw_config);
                let resolved = resolve_inputs(&with_defaults, &schema.inputs, &output_map)?;

                // Validate-with-holes: concrete leaves checked normally,
                // pending leaves checked against expected_type. Catches
                // type mismatches between upstream output type and
                // downstream input type at plan time, even when the value
                // isn't yet known.
                crate::schema::validate_resolvable(&schema.inputs, &resolved)
                    .map_err(|e| format!("resource '{name}': {e}"))?;
                res_type.validate(&resolved)?;

                let old_inputs = state.resources.get(name).map(|r| &r.inputs);
                let mut diff = diff_resource(schema, old_inputs, Some(&resolved));
                let current_outputs = state
                    .resources
                    .get(name)
                    .map(|r| &r.outputs)
                    .cloned()
                    .unwrap_or_default();
                res_type.customize_diff(&mut diff, &resolved, &current_outputs)?;

                // Optimistic default: an Updating resource's state outputs
                // continue to flow to downstream's plan-time resolution
                // unless the provider's `customize_diff` explicitly marked
                // specific outputs as recomputed by this Update (via
                // `diff.recomputed_outputs`). For Replace/Create/Delete,
                // no outputs flow — Replace recomputes everything by
                // definition, Create has no state to flow, Delete is going
                // away. Matches Terraform's CustomizeDiff + SetNewComputed
                // model and avoids the false-positive cascade where a
                // Blue-side input flip on an upstream caused every
                // downstream that referenced it via a force_new field to
                // be wrongly Replaced.
                if let Some(res_state) = state.resources.get(name) {
                    if let Some(filtered) = outputs_for_plan(&res_state.outputs, &diff) {
                        output_map.insert(node.to_string(), filtered);
                    }
                }

                diffs.insert(node.to_string(), diff);
                resolved_inputs_map.insert(node.to_string(), resolved);
            } else {
                // Deletion node — in state but not in config. Its outputs
                // are explicitly excluded from output_map (the resource is
                // being removed, so anything that referenced it is now a
                // graph error caught earlier — or it was already excluded
                // because the dependent is also being removed).
                let res_state = &state.resources[name];
                let schema = providers
                    .resource_type(&res_state.resource_type)
                    .ok_or_else(|| format!("Unknown resource type: {}", res_state.resource_type))?
                    .schema();
                let diff = diff_resource(schema, Some(&res_state.inputs), None);
                diffs.insert(node.to_string(), diff);
            }
        }
    }

    // Cascade: propagate replacements to downstream force_new dependents.
    // Operates on raw template strings in config, independent of resolution
    // state — works the same as before.
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
        known_outputs: output_map,
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

/// Decide whether and how an upstream resource's state outputs should be
/// exposed to downstream plan-time resolution, based on the upstream's
/// planned action and any recomputation overrides from `customize_diff`.
///
/// Returning `None` means the entry should NOT be inserted into the
/// `output_map` at all — downstream refs to this resource then become
/// `Resolvable::Unknown` and resolve at deploy time. (Inserting an empty
/// `Value::Object` instead would cause path-navigation errors at
/// resolution time, which the resolver would treat as real config errors
/// rather than pending — wrong semantics.)
///
/// - `Unchanged`: full state outputs flow.
/// - `Update`: optimistic — full state outputs flow when the provider
///   hasn't flagged anything as recomputed by this Update. If
///   `recomputed_outputs` is non-empty we conservatively withhold the
///   entire entry (per-field selective filtering would need resolver
///   changes to distinguish "recomputed" from "missing field"; deferred).
/// - `Replace` / `Create` / `Delete`: nothing flows. Replace recomputes
///   everything by definition, Create has no state, Delete is going away.
///   Downstream force_new refs are still promoted to Replace by
///   `cascade_replacements`; non-force_new refs become pending and
///   re-resolve at deploy time.
fn outputs_for_plan(state_outputs: &Value, diff: &Diff) -> Option<Value> {
    match diff.action {
        Action::Unchanged => Some(state_outputs.clone()),
        Action::Update => {
            if diff.recomputed_outputs.is_empty() {
                Some(state_outputs.clone())
            } else {
                None
            }
        }
        _ => None,
    }
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
            if diff.action == Action::Replace
                || diff.action == Action::Create
                || diff.action == Action::Delete
            {
                continue;
            }

            if res_def.config.is_empty() {
                continue;
            }

            let schema = providers
                .resource_type(&res_def.resource_type)
                .ok_or_else(|| format!("Unknown resource type: {}", res_def.resource_type))?
                .schema();

            let mut needs_replace = false;
            for (field_name, value) in &res_def.config {
                let is_force_new = schema
                    .inputs
                    .iter()
                    .any(|f| f.path == *field_name && f.force_new);
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

/// Refuse to plan when the config uses any resource type with `secret = true`
/// outputs but no `[encryption].recipients` are configured. Without
/// recipients, secret values would be persisted cleartext in state.
/// Fired before graph building so the user gets the error before any
/// other plan-time work.
fn refuse_if_secrets_without_recipients(
    config: &ResourceConfig,
    providers: &Providers,
) -> Result<(), String> {
    refuse_if_secrets_without_recipients_inner(config, |name| {
        providers.resource_type(name).map(|rt| rt.schema())
    })
}

/// Inner logic, parameterised on schema lookup so tests don't need a
/// full provider instance to exercise the secret/no-recipients refusal.
fn refuse_if_secrets_without_recipients_inner<'a, F>(
    config: &ResourceConfig,
    schema_for: F,
) -> Result<(), String>
where
    F: Fn(&str) -> Option<&'a crate::types::Schema>,
{
    let has_recipients = config
        .encryption
        .as_ref()
        .map(|e| !e.recipients.is_empty())
        .unwrap_or(false);
    if has_recipients {
        return Ok(());
    }
    for (name, def) in &config.resources {
        let Some(schema) = schema_for(&def.resource_type) else {
            // Unknown resource type — let downstream graph building
            // produce its own (more specific) error. Don't pre-empt.
            continue;
        };
        if schema.outputs.iter().any(|o| o.secret) {
            return Err(format!(
                "resource '{name}' (type '{rt_type}') has secret outputs but no \
                 [encryption] recipients are configured; add a [encryption] block \
                 with at least one age recipient before planning",
                rt_type = def.resource_type
            ));
        }
    }
    Ok(())
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
        let config = parse_resource_config(
            r#"
[resources.test]
type = "blue.script"
script = "test.js"
triggers_replace = { key = "value" }
"#,
        )
        .unwrap();

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
        state.resources.insert(
            "old".to_string(),
            ResourceState {
                resource_type: "blue.script".to_string(),
                inputs: json!({"script": "old.js"}),
                outputs: json!({}),
                depends_on: vec![],
            },
        );

        let providers = setup_providers();
        let plan = create_plan(&config, &state, &providers, &HashMap::new()).unwrap();

        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].name, "old");
        assert_eq!(plan.steps[0].action, Action::Delete);
    }

    #[test]
    fn plan_unchanged_resource() {
        let config = parse_resource_config(
            r#"
[resources.test]
type = "blue.script"
script = "test.js"
triggers_replace = { key = "value" }
"#,
        )
        .unwrap();

        let mut state = State::new();
        state.resources.insert(
            "test".to_string(),
            ResourceState {
                resource_type: "blue.script".to_string(),
                inputs: json!({"script": "test.js", "triggers_replace": {"key": "value"}}),
                outputs: json!({}),
                depends_on: vec![],
            },
        );

        let providers = setup_providers();
        let plan = create_plan(&config, &state, &providers, &HashMap::new()).unwrap();

        assert!(plan.steps.is_empty()); // unchanged resources don't produce steps
    }

    #[test]
    fn plan_replace_on_force_new_change() {
        let config = parse_resource_config(
            r#"
[resources.test]
type = "blue.script"
script = "new_script.js"
triggers_replace = { key = "value" }
"#,
        )
        .unwrap();

        let mut state = State::new();
        state.resources.insert(
            "test".to_string(),
            ResourceState {
                resource_type: "blue.script".to_string(),
                inputs: json!({"script": "old_script.js", "triggers_replace": {"key": "value"}}),
                outputs: json!({}),
                depends_on: vec![],
            },
        );

        let providers = setup_providers();
        let plan = create_plan(&config, &state, &providers, &HashMap::new()).unwrap();

        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].action, Action::Replace);
    }

    #[test]
    fn plan_with_parameter() {
        let config = parse_resource_config(
            r#"
[parameters.name]
default = "test-server"

[resources.test]
type = "blue.script"
script = "test.js"
triggers_replace = { name = "{{ parameters.name }}" }
"#,
        )
        .unwrap();

        let state = State::new();
        let providers = setup_providers();
        let plan = create_plan(&config, &state, &providers, &HashMap::new()).unwrap();

        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].action, Action::Create);
        // resolved_inputs is now a Resolvable; for this all-concrete test
        // it should be Known(...) and indexable via as_concrete.
        let inputs = plan.steps[0]
            .resolved_inputs
            .as_ref()
            .unwrap()
            .as_concrete()
            .expect("inputs should be fully concrete (no pending refs)");
        assert_eq!(inputs["triggers_replace"]["name"], "test-server");
    }

    // === Pending-resolution integration tests ===
    //
    // These exercise the plan-time resolver + diff + cascade interaction
    // for the scenarios that motivated the Resolvable refactor: forward
    // refs to not-yet-deployed resources, renames, multi-level chains, and
    // mixed concrete/pending fields.

    fn step_for<'a>(plan: &'a Plan, name: &str) -> &'a PlanStep {
        plan.steps
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no step for '{name}'"))
    }

    #[test]
    fn forward_ref_create_plus_create_succeeds_at_plan_time() {
        // The bucket case: brand-new upstream resource + brand-new
        // downstream resource that references its uuid. Pre-Resolvable,
        // this errored at plan time with "Ref target not found in outputs".
        let config = parse_resource_config(
            r#"
[resources.upstream]
type = "blue.script"
script = "u.js"
triggers_replace = { v = "1" }

[resources.downstream]
type = "blue.script"
script = "d.js"
triggers_replace = { up_uuid = "{{ resources.upstream.uuid }}" }
"#,
        )
        .unwrap();

        let state = State::new();
        let providers = setup_providers();
        let plan = create_plan(&config, &state, &providers, &HashMap::new()).unwrap();

        // Two steps, both Create, downstream depends on upstream.
        assert_eq!(plan.steps.len(), 2);
        let upstream = step_for(&plan, "upstream");
        let downstream = step_for(&plan, "downstream");
        assert_eq!(upstream.action, Action::Create);
        assert_eq!(downstream.action, Action::Create);
        assert!(
            downstream.depends_on.contains(&"resources.upstream".to_string()),
            "downstream should depend on upstream; got {:?}",
            downstream.depends_on,
        );

        // Downstream's resolved_inputs should NOT be fully concrete — it
        // contains a pending ref to upstream.uuid.
        let downstream_inputs = downstream.resolved_inputs.as_ref().unwrap();
        assert!(
            downstream_inputs.as_concrete().is_none(),
            "downstream inputs should still contain a pending ref"
        );
    }

    #[test]
    fn rename_create_plus_create_succeeds_at_plan_time() {
        // The rename case: an existing resource is being deleted and a
        // new one with a different name takes its place; a NEW dependent
        // references the new name. Plan should produce three steps and
        // not error out on the forward ref to the renamed resource.
        let config = parse_resource_config(
            r#"
[resources.svc_renamed]
type = "blue.script"
script = "s.js"
triggers_replace = { v = "1" }

[resources.bucket]
type = "blue.script"
script = "b.js"
triggers_replace = { svc_uuid = "{{ resources.svc_renamed.uuid }}" }
"#,
        )
        .unwrap();

        let mut state = State::new();
        state.resources.insert(
            "svc_old".to_string(),
            ResourceState {
                resource_type: "blue.script".to_string(),
                inputs: json!({"script": "s.js", "triggers_replace": {"v": "0"}}),
                outputs: json!({"x": 1}),
                depends_on: vec![],
            },
        );

        let providers = setup_providers();
        let plan = create_plan(&config, &state, &providers, &HashMap::new()).unwrap();

        // Three steps: delete old service, create renamed service, create bucket.
        assert_eq!(plan.steps.len(), 3);
        assert_eq!(step_for(&plan, "svc_old").action, Action::Delete);
        assert_eq!(step_for(&plan, "svc_renamed").action, Action::Create);
        assert_eq!(step_for(&plan, "bucket").action, Action::Create);
    }

    #[test]
    fn replace_upstream_via_force_new_input_drives_dependent_non_unchanged() {
        // Upstream's `script` is force_new on blue.script; changing it
        // (v1.js → v2.js) makes the upstream Replace. Downstream
        // references upstream's uuid via its triggers_replace (also
        // force_new). The new optimistic-default filter returns None
        // for Replace upstreams (no outputs flow), so downstream's ref
        // resolves to Unknown → diff Modified → downstream's
        // triggers_replace force_new promotes it to Replace.
        // (cascade_replacements would also catch this case; both paths
        // converge on the same answer.)
        let config = parse_resource_config(
            r#"
[resources.upstream]
type = "blue.script"
script = "v2.js"
triggers_replace = { v = "1" }

[resources.downstream]
type = "blue.script"
script = "d.js"
triggers_replace = { up_uuid = "{{ resources.upstream.uuid }}" }
"#,
        )
        .unwrap();

        let mut state = State::new();
        state.resources.insert(
            "upstream".to_string(),
            ResourceState {
                resource_type: "blue.script".to_string(),
                inputs: json!({"script": "v1.js", "triggers_replace": {"v": "1"}}),
                outputs: json!({"uuid": "old-uuid"}),
                depends_on: vec![],
            },
        );
        state.resources.insert(
            "downstream".to_string(),
            ResourceState {
                resource_type: "blue.script".to_string(),
                inputs: json!({"script": "d.js", "triggers_replace": {"up_uuid": "old-uuid"}}),
                outputs: json!({}),
                depends_on: vec!["resources.upstream".to_string()],
            },
        );

        let providers = setup_providers();
        let plan = create_plan(&config, &state, &providers, &HashMap::new()).unwrap();

        let down = step_for(&plan, "downstream");
        // Conservative: upstream is changing, downstream's ref to it is
        // treated as pending → diff is non-Unchanged. The pre-Resolvable
        // bug would have evaluated downstream's input to "old-uuid" (the
        // old upstream output) and incorrectly marked downstream as
        // Unchanged.
        assert_ne!(
            down.action,
            Action::Unchanged,
            "downstream should not be Unchanged when its upstream is changing"
        );
    }

    #[test]
    fn replace_upstream_cascades_to_dependent_via_force_new_ref() {
        // Upstream is being replaced (force_new field changed). Downstream
        // references upstream's uuid in its OWN force_new field — cascade
        // should promote downstream to Replace.
        let config = parse_resource_config(
            r#"
[resources.upstream]
type = "blue.script"
script = "v2.js"
triggers_replace = { v = "2" }

[resources.downstream]
type = "blue.script"
script = "d.js"
triggers_replace = { up_uuid = "{{ resources.upstream.uuid }}" }
"#,
        )
        .unwrap();

        let mut state = State::new();
        state.resources.insert(
            "upstream".to_string(),
            ResourceState {
                resource_type: "blue.script".to_string(),
                inputs: json!({"script": "v1.js", "triggers_replace": {"v": "1"}}),
                outputs: json!({"uuid": "old-uuid"}),
                depends_on: vec![],
            },
        );
        state.resources.insert(
            "downstream".to_string(),
            ResourceState {
                resource_type: "blue.script".to_string(),
                inputs: json!({"script": "d.js", "triggers_replace": {"up_uuid": "old-uuid"}}),
                outputs: json!({}),
                depends_on: vec!["resources.upstream".to_string()],
            },
        );

        let providers = setup_providers();
        let plan = create_plan(&config, &state, &providers, &HashMap::new()).unwrap();

        assert_eq!(step_for(&plan, "upstream").action, Action::Replace);
        // cascade_replacements should have promoted downstream to Replace
        // because its triggers_replace (force_new) refs the replaced upstream.
        assert_eq!(step_for(&plan, "downstream").action, Action::Replace);
    }

    #[test]
    fn delete_only_change_does_not_disturb_other_resources() {
        // One resource being removed from config, another unchanged.
        // The delete shouldn't produce any plan-time resolution issues.
        let config = parse_resource_config(
            r#"
[resources.keeper]
type = "blue.script"
script = "k.js"
triggers_replace = { v = "1" }
"#,
        )
        .unwrap();

        let mut state = State::new();
        state.resources.insert(
            "keeper".to_string(),
            ResourceState {
                resource_type: "blue.script".to_string(),
                inputs: json!({"script": "k.js", "triggers_replace": {"v": "1"}}),
                outputs: json!({}),
                depends_on: vec![],
            },
        );
        state.resources.insert(
            "doomed".to_string(),
            ResourceState {
                resource_type: "blue.script".to_string(),
                inputs: json!({"script": "d.js"}),
                outputs: json!({}),
                depends_on: vec![],
            },
        );

        let providers = setup_providers();
        let plan = create_plan(&config, &state, &providers, &HashMap::new()).unwrap();

        assert_eq!(plan.steps.len(), 1);
        assert_eq!(step_for(&plan, "doomed").action, Action::Delete);
    }

    #[test]
    fn multi_level_chain_all_create_succeeds() {
        // A → B → C, all being created from scratch. Each downstream
        // refs the upstream's uuid. Plan should succeed with three
        // Create steps in topological order.
        let config = parse_resource_config(
            r#"
[resources.a]
type = "blue.script"
script = "a.js"
triggers_replace = { v = "1" }

[resources.b]
type = "blue.script"
script = "b.js"
triggers_replace = { a_uuid = "{{ resources.a.uuid }}" }

[resources.c]
type = "blue.script"
script = "c.js"
triggers_replace = { b_uuid = "{{ resources.b.uuid }}" }
"#,
        )
        .unwrap();

        let providers = setup_providers();
        let plan = create_plan(&config, &State::new(), &providers, &HashMap::new()).unwrap();

        assert_eq!(plan.steps.len(), 3);
        for name in ["a", "b", "c"] {
            assert_eq!(step_for(&plan, name).action, Action::Create);
        }
        // Topological ordering: a before b before c.
        let pos = |n: &str| plan.steps.iter().position(|s| s.name == n).unwrap();
        assert!(pos("a") < pos("b"));
        assert!(pos("b") < pos("c"));
    }

    #[test]
    fn mixed_concrete_and_pending_fields_in_one_resource() {
        // Resource has two refs: one to a parameter (always resolvable
        // at plan time) and one to a not-yet-deployed resource (pending).
        // Resolved_inputs should be a Resolvable::Object with the
        // parameter resolved as Known and the resource ref as Unknown.
        let config = parse_resource_config(
            r#"
[parameters.env]
default = "prod"

[resources.upstream]
type = "blue.script"
script = "u.js"
triggers_replace = { v = "1" }

[resources.downstream]
type = "blue.script"
script = "d.js"
triggers_replace = { env = "{{ parameters.env }}", up = "{{ resources.upstream.uuid }}" }
"#,
        )
        .unwrap();

        let providers = setup_providers();
        let plan = create_plan(&config, &State::new(), &providers, &HashMap::new()).unwrap();

        let down = step_for(&plan, "downstream");
        let inputs = down.resolved_inputs.as_ref().unwrap();

        // Walk the Resolvable to find the env field — should be a
        // resolved string "prod" wrapped as Known under the
        // triggers_replace object.
        match inputs {
            Resolvable::Object(root) => {
                let triggers = root.get("triggers_replace").expect("has triggers_replace");
                match triggers {
                    Resolvable::Object(t_map) => {
                        // env was resolved against parameters
                        assert!(
                            matches!(t_map.get("env"), Some(Resolvable::Known(v)) if v == &json!("prod")),
                            "env should be resolved to Known(\"prod\")"
                        );
                        // up is still pending
                        assert!(
                            matches!(t_map.get("up"), Some(Resolvable::Unknown { .. })),
                            "up should be Unknown"
                        );
                    }
                    other => panic!("expected Object for triggers_replace, got {other:?}"),
                }
            }
            other => panic!("expected Object root, got {other:?}"),
        }
    }

    // === outputs_for_plan helper unit tests ===
    //
    // Direct unit tests on the filter function. We can't easily exercise
    // the optimistic-Update path through `create_plan` because every
    // currently registered resource type (`blue.script`) has only
    // force_new inputs, so every input change produces Replace. These
    // tests cover the action-by-action filter behavior at the helper
    // level so the contract is pinned down regardless of which providers
    // are available.

    fn diff_with(action: Action, recomputed: Vec<&str>) -> Diff {
        Diff {
            action,
            changes: vec![],
            requires_stop: false,
            recomputed_outputs: recomputed.into_iter().map(String::from).collect(),
        }
    }

    #[test]
    fn outputs_for_plan_unchanged_returns_full_state() {
        let outputs = json!({"uuid": "X", "endpoints": ["a", "b"]});
        let diff = diff_with(Action::Unchanged, vec![]);
        assert_eq!(outputs_for_plan(&outputs, &diff), Some(outputs));
    }

    #[test]
    fn outputs_for_plan_update_with_no_recomputed_returns_full_state() {
        // Optimistic default: Update upstream's outputs flow through
        // unchanged when the provider hasn't flagged any as recomputed.
        // This is what fixes the bucket case — service Update with a
        // Blue-side-only field flip leaves all outputs available.
        let outputs = json!({"uuid": "X", "endpoints": ["a"]});
        let diff = diff_with(Action::Update, vec![]);
        assert_eq!(outputs_for_plan(&outputs, &diff), Some(outputs));
    }

    #[test]
    fn outputs_for_plan_update_with_any_recomputed_withholds_entire_entry() {
        // v1: per-field selective filtering would need resolver changes
        // to distinguish "field recomputed" from "field missing." For
        // now, any recomputed entry causes the whole resource to be
        // treated as opaque to downstream. Coarse but safe: downstream's
        // refs become pending and re-resolve at deploy time.
        let outputs = json!({"uuid": "X", "endpoints": ["a"]});
        let diff = diff_with(Action::Update, vec!["endpoints"]);
        assert_eq!(outputs_for_plan(&outputs, &diff), None);
    }

    #[test]
    fn outputs_for_plan_replace_returns_none() {
        // Replace recomputes everything — no outputs flow regardless.
        let outputs = json!({"uuid": "X"});
        let diff = diff_with(Action::Replace, vec![]);
        assert_eq!(outputs_for_plan(&outputs, &diff), None);
    }

    #[test]
    fn outputs_for_plan_create_returns_none() {
        // Create has no state outputs in practice, but if called
        // (defensively) it returns None.
        let outputs = json!({});
        let diff = diff_with(Action::Create, vec![]);
        assert_eq!(outputs_for_plan(&outputs, &diff), None);
    }

    #[test]
    fn outputs_for_plan_delete_returns_none() {
        let outputs = json!({"uuid": "X"});
        let diff = diff_with(Action::Delete, vec![]);
        assert_eq!(outputs_for_plan(&outputs, &diff), None);
    }

    #[test]
    fn all_concrete_plan_remains_unchanged_when_state_matches() {
        // Regression: a fully-concrete plan with no template refs and a
        // matching state should report Unchanged exactly as before the
        // Resolvable refactor.
        let config = parse_resource_config(
            r#"
[resources.test]
type = "blue.script"
script = "test.js"
triggers_replace = { key = "value" }
"#,
        )
        .unwrap();

        let mut state = State::new();
        state.resources.insert(
            "test".to_string(),
            ResourceState {
                resource_type: "blue.script".to_string(),
                inputs: json!({"script": "test.js", "triggers_replace": {"key": "value"}}),
                outputs: json!({}),
                depends_on: vec![],
            },
        );

        let providers = setup_providers();
        let plan = create_plan(&config, &state, &providers, &HashMap::new()).unwrap();
        assert!(
            plan.steps.is_empty(),
            "Unchanged plan should have no steps; got {:?}",
            plan.steps
        );

        // known_outputs should also include the Unchanged resource's
        // outputs (so anything depending on it could see them).
        assert!(plan.known_outputs.contains_key("resources.test"));
    }

    use crate::types::{FieldType, OutputDef, Schema};

    fn schema_with_secret() -> Schema {
        Schema {
            inputs: vec![],
            outputs: vec![OutputDef {
                path: "secret_field".to_string(),
                field_type: FieldType::String,
                secret: true,
            }],
            retry: None,
            timeout: None,
        }
    }

    fn schema_without_secret() -> Schema {
        Schema {
            inputs: vec![],
            outputs: vec![OutputDef {
                path: "public_field".to_string(),
                field_type: FieldType::String,
                secret: false,
            }],
            retry: None,
            timeout: None,
        }
    }

    #[test]
    fn refuse_when_secret_resource_used_without_recipients() {
        let config = parse_resource_config(
            r#"
[resources.with_secret]
type = "test.with_secret"
"#,
        )
        .unwrap();
        let schema = schema_with_secret();
        let lookup = |name: &str| {
            if name == "test.with_secret" { Some(&schema) } else { None }
        };
        let err = refuse_if_secrets_without_recipients_inner(&config, lookup).unwrap_err();
        assert!(err.contains("with_secret"), "got: {err}");
        assert!(err.contains("[encryption]"), "got: {err}");
        assert!(err.contains("recipients"), "got: {err}");
    }

    #[test]
    fn proceed_when_recipients_configured_even_if_secret_resource_used() {
        let config = parse_resource_config(
            r#"
[encryption]
recipients = ["age1abc"]

[resources.with_secret]
type = "test.with_secret"
"#,
        )
        .unwrap();
        let schema = schema_with_secret();
        let lookup = |name: &str| {
            if name == "test.with_secret" { Some(&schema) } else { None }
        };
        refuse_if_secrets_without_recipients_inner(&config, lookup).unwrap();
    }

    #[test]
    fn proceed_when_no_resource_has_secret_outputs() {
        let config = parse_resource_config(
            r#"
[resources.public]
type = "test.public"
"#,
        )
        .unwrap();
        let schema = schema_without_secret();
        let lookup = |name: &str| {
            if name == "test.public" { Some(&schema) } else { None }
        };
        refuse_if_secrets_without_recipients_inner(&config, lookup).unwrap();
    }

    #[test]
    fn refuse_when_recipients_list_is_empty() {
        // [encryption] block present but recipients = [] should still be
        // treated as "no recipients" — refuse if any secret resource is used.
        let config = parse_resource_config(
            r#"
[encryption]
recipients = []

[resources.with_secret]
type = "test.with_secret"
"#,
        )
        .unwrap();
        let schema = schema_with_secret();
        let lookup = |name: &str| {
            if name == "test.with_secret" { Some(&schema) } else { None }
        };
        let err = refuse_if_secrets_without_recipients_inner(&config, lookup).unwrap_err();
        assert!(err.contains("with_secret"), "got: {err}");
    }
}
