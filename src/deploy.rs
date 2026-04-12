use std::collections::HashMap;
use std::path::Path;

use crate::graph::DependencyGraph;
use crate::provider::{OperationResult, ProviderRegistry};
use crate::reference::{OutputRegistry, Ref};
use crate::schema;
use crate::state::{self, PropertyChange, ResourceSnapshot, ResourceStatus, State};

fn set_resource_status(
    state: &mut State,
    name: &str,
    status: ResourceStatus,
    state_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    state.resources.get_mut(name).unwrap().status = status;
    state::save_ref(state, state_path)
}

fn save_resource(
    state: &mut State,
    name: &str,
    resource_type: &str,
    status: ResourceStatus,
    properties: &serde_json::Value,
    outputs: serde_json::Value,
    state_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    state.resources.insert(
        name.to_string(),
        ResourceSnapshot {
            resource_type: resource_type.to_string(),
            status,
            properties: properties.clone(),
            outputs,
        },
    );
    state::save_ref(state, state_path)
}

/// Resolve `{{...}}` references in properties using outputs from ready resources.
fn resolve_properties(
    properties: &serde_json::Value,
    state: &State,
    graph_registry: &OutputRegistry,
    identities: Option<&[Box<dyn age::Identity>]>,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let mut output_reg = OutputRegistry::new();

    // Include data outputs, parameters, and hook outputs from graph resolution
    output_reg.merge_from(graph_registry);

    // Include resource outputs from state (created during deploy traversal)
    for (n, snap) in &state.resources {
        if snap.status == ResourceStatus::Ready {
            if let Some(obj) = snap.outputs.as_object() {
                for (k, v) in obj {
                    output_reg.insert("resources", n, k, v.clone());
                }
            }
        }
    }

    let props_str = properties.to_string();
    let resolved_str = Ref::resolve_all(&props_str, &output_reg);
    let mut resolved: serde_json::Value = serde_json::from_str(&resolved_str)?;

    // Decrypt any encrypted secrets
    if let Some(ids) = identities {
        decrypt_secrets(&mut resolved, ids)?;
    }

    Ok(resolved)
}

fn decrypt_secrets(
    value: &mut serde_json::Value,
    identities: &[Box<dyn age::Identity>],
) -> Result<(), Box<dyn std::error::Error>> {
    match value {
        serde_json::Value::String(s) => {
            if s.contains("<encrypted:") {
                *s = decrypt_encrypted_markers(s, identities)?;
            }
        }
        serde_json::Value::Object(map) => {
            for v in map.values_mut() {
                decrypt_secrets(v, identities)?;
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                decrypt_secrets(v, identities)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Find and replace all `<encrypted:hmac:b64>` markers within a string.
fn decrypt_encrypted_markers(
    text: &str,
    identities: &[Box<dyn age::Identity>],
) -> Result<String, Box<dyn std::error::Error>> {
    use base64::Engine;
    use std::io::Read;

    let mut result = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(start) = rest.find("<encrypted:") {
        result.push_str(&rest[..start]);
        let after_open = &rest[start + "<encrypted:".len()..];

        let end = after_open
            .find('>')
            .ok_or("malformed encrypted marker: missing closing >")?;
        let inner = &after_open[..end];

        // inner is "hmac_hex:base64_ciphertext"
        if let Some((_hmac, b64)) = inner.split_once(':') {
            let ciphertext = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| format!("failed to decode encrypted value: {e}"))?;

            let identity_refs: Vec<&dyn age::Identity> = identities
                .iter()
                .map(|i| i.as_ref() as &dyn age::Identity)
                .collect();

            let decryptor = age::Decryptor::new(&ciphertext[..])
                .map_err(|e| format!("failed to create decryptor: {e}"))?;

            let mut reader = decryptor
                .decrypt(identity_refs.into_iter())
                .map_err(|e| format!("failed to decrypt: {e}"))?;

            let mut decrypted = vec![];
            reader
                .read_to_end(&mut decrypted)
                .map_err(|e| format!("failed to read decrypted data: {e}"))?;

            let plaintext = String::from_utf8(decrypted)
                .map_err(|e| format!("decrypted value is not valid UTF-8: {e}"))?;

            result.push_str(&plaintext);
        } else {
            // Malformed, leave as-is
            result.push_str(&rest[start..start + "<encrypted:".len() + end + 1]);
        }

        rest = &after_open[end + 1..];
    }
    result.push_str(rest);
    Ok(result)
}

/// Load age identities from a file path.
pub fn load_identities(
    identity_path: &Path,
) -> Result<Vec<Box<dyn age::Identity>>, Box<dyn std::error::Error>> {
    let id_file = age::IdentityFile::from_file(identity_path.to_string_lossy().to_string())
        .map_err(|e| format!("failed to read identity file '{}': {e}", identity_path.display()))?;
    id_file
        .into_identities()
        .map_err(|e| format!("failed to parse identities: {e}").into())
}

/// Apply property changes to produce new properties.
fn apply_property_changes(
    base: &serde_json::Value,
    changes: &[PropertyChange],
) -> serde_json::Value {
    let mut result = base.clone();
    for change in changes {
        if let Some(obj) = result.as_object_mut() {
            match change {
                PropertyChange::Added {
                    field, new_value, ..
                }
                | PropertyChange::Modified {
                    field, new_value, ..
                } => {
                    obj.insert(field.clone(), new_value.clone());
                }
                PropertyChange::Removed { field, .. } => {
                    obj.remove(field);
                }
            }
        }
    }
    result
}

pub fn execute(
    changeset: &state::Changeset,
    state: &mut State,
    registry: &mut ProviderRegistry,
    state_path: &Path,
    graph_registry: &OutputRegistry,
    identities: Option<&[Box<dyn age::Identity>]>,
    recipients: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    // Save data snapshots first
    state.data = changeset.data_snapshots.clone();

    let graph = DependencyGraph::build_from_snapshots(&changeset.resource_snapshots)?;
    let order = graph.topological_sort_names()?;

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
                state::ResourceChange::Update {
                    name,
                    resource_type,
                    changes,
                } => {
                    update_resource(name, resource_type, changes, state, registry, state_path, graph_registry, identities, recipients)?;
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
                state::ResourceChange::Create { resource_type, .. }
                | state::ResourceChange::Replace { resource_type, .. } => {
                    let props = &changeset.resource_snapshots[name].properties;
                    create_resource(name, resource_type, props, state, registry, state_path, graph_registry, identities, recipients)?;
                }
                state::ResourceChange::Update { .. } => {
                    // Updates are fully handled in phase 1
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
    graph_registry: &OutputRegistry,
    identities: Option<&[Box<dyn age::Identity>]>,
    recipients: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let resolved_props = resolve_properties(properties, state, graph_registry, identities)?;
    // Resolve refs in properties for state storage (without decrypting secrets)
    let state_props = resolve_properties(properties, state, graph_registry, None)?;

    println!("Creating {name} ({resource_type})...");

    let result = registry.create_resource(resource_type, resolved_props)?;

    match result {
        OperationResult::Complete { outputs } => {
            let mut extracted = extract_resource_outputs(resource_type, &outputs, registry)?;
            encrypt_secret_outputs(name, resource_type, &mut extracted, registry, recipients)?;
            save_resource(
                state,
                name,
                resource_type,
                ResourceStatus::Ready,
                &state_props,
                extracted,
                state_path,
            )?;
            println!("  {name}: created");
        }
        OperationResult::InProgress { outputs } => {
            let mut extracted = extract_resource_outputs(resource_type, &outputs, registry)?;
            encrypt_secret_outputs(name, resource_type, &mut extracted, registry, recipients)?;
            save_resource(
                state,
                name,
                resource_type,
                ResourceStatus::Creating,
                &state_props,
                extracted,
                state_path,
            )?;
            poll_until_ready(name, resource_type, state, registry, state_path)?;
        }
        OperationResult::Updating { outputs } => {
            let mut extracted = extract_resource_outputs(resource_type, &outputs, registry)?;
            encrypt_secret_outputs(name, resource_type, &mut extracted, registry, recipients)?;
            save_resource(
                state,
                name,
                resource_type,
                ResourceStatus::Ready,
                &state_props,
                extracted,
                state_path,
            )?;
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

    set_resource_status(state, name, ResourceStatus::Deleting, state_path)?;

    println!("Deleting {name} ({resource_type})...");

    let result = registry.delete_resource(&resource_type, &outputs)?;

    match result {
        OperationResult::Complete { .. } => {
            state.resources.remove(name);
            state::save_ref(state, state_path)?;
            println!("  {name}: deleted");
        }
        OperationResult::Failed { error } => {
            set_resource_status(state, name, ResourceStatus::Failed, state_path)?;
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
    let snap = state
        .resources
        .get(name)
        .ok_or_else(|| format!("Resource {name} not found in state"))?;

    if snap.status == ResourceStatus::Stopping || snap.status == ResourceStatus::Stopped {
        // Already stopping or stopped, continue
        return Ok(());
    }

    set_resource_status(state, name, ResourceStatus::Stopping, state_path)?;

    println!("Stopping {name} ({resource_type})...");

    let start = std::time::Instant::now();
    loop {
        std::thread::sleep(std::time::Duration::from_secs(10));
        println!("  {name}: stopping... (current: Stopping, desired: Stopped)");

        if start.elapsed() > std::time::Duration::from_secs(30) {
            set_resource_status(state, name, ResourceStatus::Ready, state_path)?;
            return Err(format!("{name}: stop operation timed out").into());
        }

        // TODO: Check actual server state via provider API and break when stopped
    }
}

fn start_resource(
    name: &str,
    resource_type: &str,
    state: &mut State,
    _registry: &mut ProviderRegistry,
    state_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let snap = state
        .resources
        .get(name)
        .ok_or_else(|| format!("Resource {name} not found in state"))?;

    if snap.status == ResourceStatus::Starting || snap.status == ResourceStatus::Ready {
        // Already starting or ready
        return Ok(());
    }

    set_resource_status(state, name, ResourceStatus::Starting, state_path)?;

    println!("Starting {name} ({resource_type})...");

    let start = std::time::Instant::now();
    loop {
        std::thread::sleep(std::time::Duration::from_secs(10));
        println!("  {name}: starting... (current: Starting, desired: Ready)");

        if start.elapsed() > std::time::Duration::from_secs(30) {
            set_resource_status(state, name, ResourceStatus::Failed, state_path)?;
            return Err(format!("{name}: start operation timed out").into());
        }

        // TODO: Check actual server state via provider API and break when started
    }
}

fn update_resource(
    name: &str,
    resource_type: &str,
    property_changes: &[PropertyChange],
    state: &mut State,
    registry: &mut ProviderRegistry,
    state_path: &Path,
    graph_registry: &OutputRegistry,
    identities: Option<&[Box<dyn age::Identity>]>,
    recipients: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    // Check if this update requires server stoppage
    let requires_stop = requires_stop_for_update(resource_type, property_changes, registry)?;

    let old_snapshot = state
        .resources
        .get(name)
        .ok_or_else(|| format!("Resource {name} not found in state"))?;
    let old_outputs = old_snapshot.outputs.clone();
    let new_properties = apply_property_changes(&old_snapshot.properties, property_changes);
    let resolved_props = resolve_properties(&new_properties, state, graph_registry, identities)?;
    let state_props = resolve_properties(&new_properties, state, graph_registry, None)?;

    if requires_stop {
        println!("Updating {name} ({resource_type}) - stop required...");

        // Stop the resource first
        stop_resource(name, resource_type, state, registry, state_path)?;

        set_resource_status(state, name, ResourceStatus::Updating, state_path)?;

        let start = std::time::Instant::now();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(10));
            println!("  {name}: updating... (current: Updating, desired: Ready)");

            if start.elapsed() > std::time::Duration::from_secs(30) {
                set_resource_status(state, name, ResourceStatus::Failed, state_path)?;
                return Err(format!("{name}: update operation timed out").into());
            }

            // TODO: Check actual update progress via provider API
            break;
        }
    } else {
        println!("Updating {name} ({resource_type})...");
    }

    let result = registry.update_resource(resource_type, &old_outputs, resolved_props)?;

    let (outputs, msg) = match result {
        OperationResult::Complete { outputs } | OperationResult::Updating { outputs } => {
            (outputs, "updated")
        }
        OperationResult::InProgress { outputs } => (outputs, "update in progress"),
        OperationResult::Failed { error } => {
            if requires_stop {
                if let Err(start_error) =
                    start_resource(name, resource_type, state, registry, state_path)
                {
                    return Err(format!(
                        "Update failed for {name}: {error}, and restart failed: {start_error}"
                    )
                    .into());
                }
            }
            return Err(format!("Update failed for {name}: {error}").into());
        }
    };

    let mut extracted = extract_resource_outputs(resource_type, &outputs, registry)?;
    preserve_secret_outputs(&mut extracted, &old_outputs, resource_type, registry)?;
    encrypt_secret_outputs(name, resource_type, &mut extracted, registry, recipients)?;
    if let Some(snapshot) = state.resources.get_mut(name) {
        snapshot.properties = state_props;
        snapshot.outputs = extracted;
        snapshot.status = if msg == "update in progress" && requires_stop {
            ResourceStatus::Updating
        } else {
            ResourceStatus::Ready
        };
    }

    if requires_stop && msg != "update in progress" {
        start_resource(name, resource_type, state, registry, state_path)?;
        set_resource_status(state, name, ResourceStatus::Ready, state_path)?;
    } else {
        state::save_ref(state, state_path)?;
    }

    println!("  {name}: {msg}");
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
            set_resource_status(state, name, ResourceStatus::Failed, state_path)?;
            return Err(format!("{name}: timed out waiting for resource to be ready").into());
        }

        let old_outputs = state.resources[name].outputs.clone();
        let result = registry.read_resource(resource_type, &old_outputs)?;

        match result {
            OperationResult::Complete { outputs } => {
                let mut extracted = extract_resource_outputs(resource_type, &outputs, registry)?;
                preserve_secret_outputs(&mut extracted, &old_outputs, resource_type, registry)?;
                let snap = state.resources.get_mut(name).unwrap();
                snap.status = ResourceStatus::Ready;
                snap.outputs = extracted;
                state::save_ref(state, state_path)?;
                println!("  {name}: ready");
                return Ok(());
            }
            OperationResult::InProgress { outputs } | OperationResult::Updating { outputs } => {
                let mut extracted = extract_resource_outputs(resource_type, &outputs, registry)?;
                preserve_secret_outputs(&mut extracted, &old_outputs, resource_type, registry)?;
                state.resources.get_mut(name).unwrap().outputs = extracted;
                state::save_ref(state, state_path)?;
            }
            OperationResult::Failed { error } => {
                set_resource_status(state, name, ResourceStatus::Failed, state_path)?;
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
            let map: serde_json::Map<String, serde_json::Value> =
                extracted.into_iter().map(|(k, v)| (k, v)).collect();
            Ok(serde_json::Value::Object(map))
        }
        None => Ok(raw_outputs.clone()),
    }
}

/// Encrypt output fields marked as `secret` in the schema.
fn encrypt_secret_outputs(
    resource_name: &str,
    resource_type: &str,
    outputs: &mut serde_json::Value,
    registry: &mut ProviderRegistry,
    recipients: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    if recipients.is_empty() {
        return Ok(());
    }
    let schema = registry.resource_schema(resource_type)?;
    let Some(schema) = schema else { return Ok(()) };

    if let serde_json::Value::Object(map) = outputs {
        for output_def in &schema.outputs {
            if output_def.secret {
                if let Some(serde_json::Value::String(val)) = map.get(&output_def.path) {
                    let hmac_key = format!("{resource_name}.{}", output_def.path);
                    if let Some(encrypted) = state::encrypt_value(&hmac_key, val, recipients) {
                        map.insert(
                            output_def.path.clone(),
                            serde_json::Value::String(encrypted),
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

/// Preserve secret outputs from old state when the API read doesn't return them.
fn preserve_secret_outputs(
    outputs: &mut serde_json::Value,
    old_outputs: &serde_json::Value,
    resource_type: &str,
    registry: &mut ProviderRegistry,
) -> Result<(), Box<dyn std::error::Error>> {
    let schema = registry.resource_schema(resource_type)?;
    let Some(schema) = schema else { return Ok(()) };

    if let (serde_json::Value::Object(new_map), serde_json::Value::Object(old_map)) =
        (outputs, old_outputs)
    {
        for output_def in &schema.outputs {
            if output_def.secret && !new_map.contains_key(&output_def.path) {
                if let Some(old_val) = old_map.get(&output_def.path) {
                    new_map.insert(output_def.path.clone(), old_val.clone());
                }
            }
        }
    }
    Ok(())
}
