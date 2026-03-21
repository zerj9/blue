use std::collections::HashMap;
use std::path::Path;

use crate::config;
use crate::graph::DependencyGraph;
use crate::provider::{OperationResult, ProviderRegistry};
use crate::schema;
use crate::state::{self, ResourceSnapshot, ResourceStatus, State};

pub fn execute(
    changeset: &state::Changeset,
    state: &mut State,
    registry: &mut ProviderRegistry,
    state_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // Save data snapshots first
    state.data = changeset.data_snapshots.clone();

    let graph = DependencyGraph::build_from_snapshots(&changeset.resource_snapshots)?;
    let order = graph.topological_sort()?;

    // Collect changes by name for lookup
    let changes_by_name: HashMap<&str, &state::ResourceChange> = changeset
        .resource_changes
        .iter()
        .filter_map(|c| match c {
            state::ResourceChange::Create { name, .. } => Some((name.as_str(), c)),
            state::ResourceChange::Delete { name, .. } => Some((name.as_str(), c)),
            state::ResourceChange::Update { name, .. } => Some((name.as_str(), c)),
            state::ResourceChange::Replace { name, .. } => Some((name.as_str(), c)),
            state::ResourceChange::Unchanged { .. } => None,
        })
        .collect();

    // Phase 1: Deletes (reverse dependency order)
    for name in order.iter().rev() {
        if let Some(change) = changes_by_name.get(name.as_str()) {
            match change {
                state::ResourceChange::Delete { name, .. } => {
                    delete_resource(name, state, registry, state_path)?;
                }
                state::ResourceChange::Replace { name, .. } => {
                    delete_resource(name, state, registry, state_path)?;
                }
                state::ResourceChange::Update { name, .. } => {
                    // No update — delete + create
                    delete_resource(name, state, registry, state_path)?;
                }
                _ => {}
            }
        }
    }

    // Also delete resources that aren't in the new order (removed from config)
    let order_set: std::collections::HashSet<&str> = order.iter().map(|s| s.as_str()).collect();
    for change in &changeset.resource_changes {
        if let state::ResourceChange::Delete { name, .. } = change
            && !order_set.contains(name.as_str())
        {
            delete_resource(name, state, registry, state_path)?;
        }
    }

    // Phase 2: Creates (forward dependency order)
    for name in &order {
        if let Some(
            state::ResourceChange::Create {
                name,
                resource_type,
                ..
            }
            | state::ResourceChange::Replace {
                name,
                resource_type,
                ..
            }
            | state::ResourceChange::Update {
                name,
                resource_type,
                ..
            },
        ) = changes_by_name.get(name.as_str())
        {
            let props = &changeset.resource_snapshots[name].properties;
            create_resource(name, resource_type, props, state, registry, state_path)?;
        }
    }

    Ok(())
}

fn create_resource(
    name: &str,
    resource_type: &str,
    properties: &serde_json::Value,
    state: &mut State,
    registry: &mut ProviderRegistry,
    state_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // Resolve resource references in properties
    let resource_outputs: HashMap<String, serde_json::Value> = state
        .resources
        .iter()
        .filter(|(_, snap)| snap.status == ResourceStatus::Ready)
        .map(|(n, snap)| (n.clone(), snap.outputs.clone()))
        .collect();

    let props_str = properties.to_string();
    let resolved_str = config::resolve_resource_refs(&props_str, &resource_outputs)?;
    let resolved_props: serde_json::Value = serde_json::from_str(&resolved_str)?;

    println!("Creating {name} ({resource_type})...");

    let result = registry.create_resource(resource_type, resolved_props)?;

    match result {
        OperationResult::Complete { outputs } => {
            let extracted = extract_resource_outputs(resource_type, &outputs, registry)?;
            state.resources.insert(
                name.to_string(),
                ResourceSnapshot {
                    resource_type: resource_type.to_string(),
                    status: ResourceStatus::Ready,
                    properties: properties.clone(),
                    outputs: extracted,
                },
            );
            state::save_ref(state, state_path)?;
            println!("  {name}: created");
        }
        OperationResult::InProgress { outputs } => {
            let extracted = extract_resource_outputs(resource_type, &outputs, registry)?;
            state.resources.insert(
                name.to_string(),
                ResourceSnapshot {
                    resource_type: resource_type.to_string(),
                    status: ResourceStatus::Creating,
                    properties: properties.clone(),
                    outputs: extracted,
                },
            );
            state::save_ref(state, state_path)?;
            poll_until_ready(name, resource_type, state, registry, state_path)?;
        }
        OperationResult::Failed { error } => {
            return Err(format!("{name}: create failed: {error}").into());
        }
    }

    Ok(())
}

pub fn delete_resource(
    name: &str,
    state: &mut State,
    registry: &mut ProviderRegistry,
    state_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let snap = match state.resources.get(name) {
        Some(s) => s,
        None => return Ok(()), // Already gone
    };

    let resource_type = snap.resource_type.clone();
    let outputs = snap.outputs.clone();

    // Mark as Deleting
    let snap = state.resources.get_mut(name).unwrap();
    snap.status = ResourceStatus::Deleting;
    state::save_ref(state, state_path)?;

    println!("Deleting {name} ({resource_type})...");

    let result = registry.delete_resource(&resource_type, &outputs)?;

    match result {
        OperationResult::Complete { .. } => {
            state.resources.remove(name);
            state::save_ref(state, state_path)?;
            println!("  {name}: deleted");
        }
        OperationResult::Failed { error } => {
            let snap = state.resources.get_mut(name).unwrap();
            snap.status = ResourceStatus::Failed;
            state::save_ref(state, state_path)?;
            return Err(format!("{name}: delete failed: {error}").into());
        }
        OperationResult::InProgress { .. } => {
            // Shouldn't happen for delete, but handle gracefully
            state.resources.remove(name);
            state::save_ref(state, state_path)?;
            println!("  {name}: deleted");
        }
    }

    Ok(())
}

fn poll_until_ready(
    name: &str,
    resource_type: &str,
    state: &mut State,
    registry: &mut ProviderRegistry,
    state_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let poll_interval = std::time::Duration::from_secs(5);
    let timeout = std::time::Duration::from_secs(300);
    let start = std::time::Instant::now();

    println!("  {name}: waiting for ready...");

    loop {
        std::thread::sleep(poll_interval);

        if start.elapsed() > timeout {
            let snap = state.resources.get_mut(name).unwrap();
            snap.status = ResourceStatus::Failed;
            state::save_ref(state, state_path)?;
            return Err(format!("{name}: timed out waiting for resource to be ready").into());
        }

        let outputs = &state.resources[name].outputs;
        let result = registry.read_resource(resource_type, outputs)?;

        match result {
            OperationResult::Complete { outputs } => {
                let extracted = extract_resource_outputs(resource_type, &outputs, registry)?;
                let snap = state.resources.get_mut(name).unwrap();
                snap.status = ResourceStatus::Ready;
                snap.outputs = extracted;
                state::save_ref(state, state_path)?;
                println!("  {name}: ready");
                return Ok(());
            }
            OperationResult::InProgress { outputs } => {
                let extracted = extract_resource_outputs(resource_type, &outputs, registry)?;
                let snap = state.resources.get_mut(name).unwrap();
                snap.outputs = extracted;
                state::save_ref(state, state_path)?;
            }
            OperationResult::Failed { error } => {
                let snap = state.resources.get_mut(name).unwrap();
                snap.status = ResourceStatus::Failed;
                state::save_ref(state, state_path)?;
                return Err(format!("{name}: {error}").into());
            }
        }
    }
}

fn extract_resource_outputs(
    resource_type: &str,
    raw_outputs: &serde_json::Value,
    registry: &mut ProviderRegistry,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let schema = registry.resource_schema(resource_type)?;
    match schema {
        Some(s) => {
            let extracted = schema::extract_outputs(raw_outputs, &s.outputs)?;
            // Convert HashMap<String, String> to JSON object
            let map: serde_json::Map<String, serde_json::Value> = extracted
                .into_iter()
                .map(|(k, v)| (k, serde_json::Value::String(v)))
                .collect();
            Ok(serde_json::Value::Object(map))
        }
        None => Ok(raw_outputs.clone()),
    }
}
