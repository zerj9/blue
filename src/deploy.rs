use std::collections::HashMap;
use std::path::Path;

use crate::config;
use crate::graph::DependencyGraph;
use crate::provider::{OperationResult, ProviderRegistry};
use crate::schema;
use crate::state::{self, PropertyChange, ResourceSnapshot, ResourceStatus, State};

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
                state::ResourceChange::Update { name, resource_type, changes } => {
                    // Try in-place update first
                    if let Err(_) = update_resource(name, resource_type, changes, state, registry, state_path) {
                        // If update fails or is not supported, fall back to delete + create
                        delete_resource(name, state, registry, state_path)?;
                    }
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

    // Phase 2: Creates and Updates (forward dependency order)
    for name in &order {
        if let Some(change) = changes_by_name.get(name.as_str()) {
            match change {
                state::ResourceChange::Create {
                    resource_type,
                    ..
                } | state::ResourceChange::Replace {
                    resource_type,
                    ..
                } => {
                    let props = &changeset.resource_snapshots[name].properties;
                    create_resource(name, resource_type, props, state, registry, state_path)?;
                }
                state::ResourceChange::Update {
                    resource_type,
                    changes: _,
                    ..
                } => {
                    // Skip here - updates are handled in phase 1
                    // If update failed and fell back to delete, it will be recreated here
                    if !state.resources.contains_key(name) {
                        // Resource was deleted in phase 1, now recreate it
                        let props = &changeset.resource_snapshots[name].properties;
                        create_resource(name, resource_type, props, state, registry, state_path)?;
                    }
                    // Otherwise, the resource was successfully updated in phase 1
                }
                _ => {}
            }
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
        OperationResult::Updating { outputs } => {
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
            println!("  {name}: created (updating)");
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
        OperationResult::InProgress { .. } | OperationResult::Updating { .. } => {
            // Shouldn't happen for delete, but handle gracefully
            state.resources.remove(name);
            state::save_ref(state, state_path)?;
            println!("  {name}: deleted");
        }
    }

    Ok(())
}

fn requires_stop_for_update(
    resource_type: &str,
    property_changes: &[PropertyChange],
    registry: &mut ProviderRegistry,
) -> Result<bool, Box<dyn std::error::Error>> {
    let schema = registry.resource_schema(resource_type)?;
    if let Some(schema) = schema {
        for change in property_changes {
            if let PropertyChange::Modified { field, .. } = change {
                if schema.requires_stop(field) {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

fn stop_resource(
    name: &str,
    resource_type: &str,
    state: &mut State,
    _registry: &mut ProviderRegistry,
    state_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let snap = state.resources.get(name)
        .ok_or_else(|| format!("Resource {name} not found in state"))?;
    
    if snap.status == ResourceStatus::Stopping || snap.status == ResourceStatus::Stopped {
        // Already stopping or stopped, continue
        return Ok(());
    }
    
    // Mark as Stopping
    let snap = state.resources.get_mut(name).unwrap();
    snap.status = ResourceStatus::Stopping;
    state::save_ref(state, state_path)?;
    
    println!("Stopping {name} ({resource_type})...");
    
    // Log progress every 10 seconds
    let start = std::time::Instant::now();
    loop {
        std::thread::sleep(std::time::Duration::from_secs(10));
        println!("  {name}: stopping... (current: Stopping, desired: Stopped)");
        
        // Check if stopped (provider-specific logic would go here)
        // For now, we'll simulate this with a timeout
        if start.elapsed() > std::time::Duration::from_secs(30) {
            let snap = state.resources.get_mut(name).unwrap();
            snap.status = ResourceStatus::Ready; // Return to ready on timeout for now
            state::save_ref(state, state_path)?;
            return Err(format!("{name}: stop operation timed out").into());
        }
        
        // In a real implementation, we would check the actual server state
        // via the provider API and break when stopped
    }
}

fn start_resource(
    name: &str,
    resource_type: &str,
    state: &mut State,
    _registry: &mut ProviderRegistry,
    state_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let snap = state.resources.get(name)
        .ok_or_else(|| format!("Resource {name} not found in state"))?;
    
    if snap.status == ResourceStatus::Starting || snap.status == ResourceStatus::Ready {
        // Already starting or ready
        return Ok(());
    }
    
    // Mark as Starting
    let snap = state.resources.get_mut(name).unwrap();
    snap.status = ResourceStatus::Starting;
    state::save_ref(state, state_path)?;
    
    println!("Starting {name} ({resource_type})...");
    
    // Log progress every 10 seconds
    let start = std::time::Instant::now();
    loop {
        std::thread::sleep(std::time::Duration::from_secs(10));
        println!("  {name}: starting... (current: Starting, desired: Ready)");
        
        // Check if started (provider-specific logic would go here)
        if start.elapsed() > std::time::Duration::from_secs(30) {
            let snap = state.resources.get_mut(name).unwrap();
            snap.status = ResourceStatus::Failed;
            state::save_ref(state, state_path)?;
            return Err(format!("{name}: start operation timed out").into());
        }
        
        // In a real implementation, we would check the actual server state
        // via the provider API and break when started
    }
}

fn update_resource(
    name: &str,
    resource_type: &str,
    property_changes: &[PropertyChange],
    state: &mut State,
    registry: &mut ProviderRegistry,
    state_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // Check if this update requires server stoppage
    let requires_stop = requires_stop_for_update(resource_type, property_changes, registry)?;
    
    // Get the current resource snapshot and clone what we need
    let old_snapshot = state.resources.get(name)
        .ok_or_else(|| format!("Resource {name} not found in state"))?;
    let old_outputs = old_snapshot.outputs.clone();
    let old_properties = old_snapshot.properties.clone();
    
    // Apply property changes to create new properties
    let mut new_properties = old_properties.clone();
    
    for change in property_changes {
        match change {
            PropertyChange::Added { field, new_value } => {
                // Add new field
                if let Some(obj) = new_properties.as_object_mut() {
                    obj.insert(field.clone(), new_value.clone());
                }
            }
            PropertyChange::Removed { field, .. } => {
                // Remove field
                if let Some(obj) = new_properties.as_object_mut() {
                    obj.remove(field);
                }
            }
            PropertyChange::Modified { field, new_value, .. } => {
                // Update field
                if let Some(obj) = new_properties.as_object_mut() {
                    obj.insert(field.clone(), new_value.clone());
                }
            }
        }
    }

    // Resolve resource references in the new properties
    let resource_outputs: HashMap<String, serde_json::Value> = state
        .resources
        .iter()
        .filter(|(_, snap)| snap.status == ResourceStatus::Ready)
        .map(|(n, snap)| (n.clone(), snap.outputs.clone()))
        .collect();

    let props_str = new_properties.to_string();
    let resolved_str = config::resolve_resource_refs(&props_str, &resource_outputs)?;
    let resolved_props: serde_json::Value = serde_json::from_str(&resolved_str)?;

    if requires_stop {
        println!("Updating {name} ({resource_type}) - stop required...");
        
        // Stop the resource first
        stop_resource(name, resource_type, state, registry, state_path)?;
        
        // Mark as Updating
        {
            let snap = state.resources.get_mut(name).unwrap();
            snap.status = ResourceStatus::Updating;
            state::save_ref(state, state_path)?;
        }
        
        // Log progress every 10 seconds during update
        let start = std::time::Instant::now();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(10));
            println!("  {name}: updating... (current: Updating, desired: Ready)");
            
            if start.elapsed() > std::time::Duration::from_secs(30) {
                let snap = state.resources.get_mut(name).unwrap();
                snap.status = ResourceStatus::Failed;
                state::save_ref(state, state_path)?;
                return Err(format!("{name}: update operation timed out").into());
            }
            
            // In real implementation, check actual update progress
            // For now we'll proceed to the actual update call
            break;
        }
    } else {
        println!("Updating {name} ({resource_type})...");
    }

    let result = registry.update_resource(resource_type, &old_outputs, resolved_props)?;

    match result {
        OperationResult::Complete { outputs } => {
            let extracted = extract_resource_outputs(resource_type, &outputs, registry)?;
            
            // Update the resource state
            {
                if let Some(snapshot) = state.resources.get_mut(name) {
                    snapshot.properties = new_properties;
                    snapshot.outputs = extracted;
                }
            }
            
            if requires_stop {
                // Start the resource after successful update
                start_resource(name, resource_type, state, registry, state_path)?;
                // Update status to Ready after successful start
                if let Some(snapshot) = state.resources.get_mut(name) {
                    snapshot.status = ResourceStatus::Ready;
                }
            } else {
                if let Some(snapshot) = state.resources.get_mut(name) {
                    snapshot.status = ResourceStatus::Ready;
                }
            }
            
            state::save_ref(state, state_path)?;
            println!("  {name}: updated");
            Ok(())
        }
        OperationResult::Updating { outputs } => {
            let extracted = extract_resource_outputs(resource_type, &outputs, registry)?;
            
            // Update the resource state
            {
                if let Some(snapshot) = state.resources.get_mut(name) {
                    snapshot.properties = new_properties;
                    snapshot.outputs = extracted;
                }
            }
            
            if requires_stop {
                // Start the resource after successful update
                start_resource(name, resource_type, state, registry, state_path)?;
                // Update status to Ready after successful start
                if let Some(snapshot) = state.resources.get_mut(name) {
                    snapshot.status = ResourceStatus::Ready;
                }
            } else {
                if let Some(snapshot) = state.resources.get_mut(name) {
                    snapshot.status = ResourceStatus::Ready;
                }
            }
            
            state::save_ref(state, state_path)?;
            println!("  {name}: update in progress");
            Ok(())
        }
        OperationResult::InProgress { outputs } => {
            let extracted = extract_resource_outputs(resource_type, &outputs, registry)?;
            
            // Update the resource state
            {
                if let Some(snapshot) = state.resources.get_mut(name) {
                    snapshot.properties = new_properties;
                    snapshot.outputs = extracted;
                    
                    if requires_stop {
                        // For in-progress updates that require stop, we need to handle this differently
                        // This would typically be handled by polling in a real implementation
                        snapshot.status = ResourceStatus::Updating;
                    } else {
                        snapshot.status = ResourceStatus::Ready;
                    }
                }
            }
            
            state::save_ref(state, state_path)?;
            println!("  {name}: update in progress");
            Ok(())
        }
        OperationResult::Failed { error } => {
            // If update failed and we stopped the server, try to restart it
            if requires_stop {
                if let Err(start_error) = start_resource(name, resource_type, state, registry, state_path) {
                    return Err(format!("Update failed for {name}: {error}, and restart failed: {start_error}").into());
                }
            }
            Err(format!("Update failed for {name}: {error}").into())
        }
    }
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
            OperationResult::Updating { outputs } => {
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
            // Convert HashMap<String, serde_json::Value> to JSON object
            let map: serde_json::Map<String, serde_json::Value> = extracted
                .into_iter()
                .map(|(k, v)| (k, v))
                .collect();
            Ok(serde_json::Value::Object(map))
        }
        None => Ok(raw_outputs.clone()),
    }
}
