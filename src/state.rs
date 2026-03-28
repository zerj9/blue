use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{config, config::DataSource, provider::ProviderRegistry};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct State {
    pub version: u64,
    pub serial: u64,
    pub data: HashMap<String, DataSnapshot>,
    pub resources: HashMap<String, ResourceSnapshot>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct DataSnapshot {
    #[serde(rename = "type")]
    pub source_type: String,
    pub filters: HashMap<String, String>,
    pub values: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ResourceStatus {
    Creating,
    Ready,
    Failed,
    Deleting,
    Stopping,
    Stopped,
    Updating,
    Starting,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct ResourceSnapshot {
    #[serde(rename = "type")]
    pub resource_type: String,
    pub status: ResourceStatus,
    pub properties: serde_json::Value,
    pub outputs: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum PropertyChange {
    Added {
        field: String,
        new_value: serde_json::Value,
    },
    Removed {
        field: String,
        old_value: serde_json::Value,
    },
    Modified {
        field: String,
        old_value: serde_json::Value,
        new_value: serde_json::Value,
        force_new: bool,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ResourceChange {
    Create {
        name: String,
        resource_type: String,
        properties: serde_json::Value,
    },
    Delete {
        name: String,
        resource_type: String,
    },
    Update {
        name: String,
        resource_type: String,
        changes: Vec<PropertyChange>,
    },
    Replace {
        name: String,
        resource_type: String,
        changes: Vec<PropertyChange>,
    },
    Unchanged {
        name: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum DataChange {
    Added(String),
    Removed(String),
    Changed {
        source: String,
        key: String,
        old_value: serde_json::Value,
        new_value: serde_json::Value,
    },
    FiltersChanged {
        source: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Changeset {
    pub version: u64,
    pub base_serial: u64,
    pub data_snapshots: HashMap<String, DataSnapshot>,
    pub resource_snapshots: HashMap<String, ResourceSnapshot>,
    pub data_changes: Vec<DataChange>,
    pub resource_changes: Vec<ResourceChange>,
}

impl State {
    fn empty() -> Self {
        Self {
            version: 1,
            serial: 0,
            data: HashMap::new(),
            resources: HashMap::new(),
        }
    }
}

pub fn load(path: &Path) -> Result<State, Box<dyn std::error::Error>> {
    if !path.exists() {
        return Ok(State::empty());
    }
    let contents = std::fs::read_to_string(path)?;
    let state: State = serde_json::from_str(&contents)?;
    if state.version != 1 {
        return Err(format!(
            "unsupported state file version: {} (expected 1)",
            state.version
        )
        .into());
    }
    Ok(state)
}

pub fn save(mut state: State, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    state.serial += 1;
    let json = serde_json::to_string_pretty(&state)?;

    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn save_ref(state: &mut State, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    state.serial += 1;
    let json = serde_json::to_string_pretty(&state)?;

    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn toml_to_json(value: &toml::Value) -> serde_json::Value {
    match value {
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Integer(n) => serde_json::json!(*n),
        toml::Value::Float(f) => serde_json::json!(*f),
        toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
        toml::Value::Array(arr) => serde_json::Value::Array(arr.iter().map(toml_to_json).collect()),
        toml::Value::Table(table) => {
            let mut map = serde_json::Map::new();
            for (k, v) in table {
                map.insert(k.clone(), toml_to_json(v));
            }
            serde_json::Value::Object(map)
        }
        toml::Value::Datetime(d) => serde_json::Value::String(d.to_string()),
    }
}

pub fn flatten_json(
    prefix: &str,
    value: &serde_json::Value,
    out: &mut Vec<(String, serde_json::Value)>,
) {
    if let serde_json::Value::Object(map) = value {
        for (key, val) in map {
            let path = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            if val.is_object() {
                flatten_json(&path, val, out);
            } else {
                out.push((path, val.clone()));
            }
        }
    }
}

pub fn save_changeset(
    changeset: &Changeset,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let json = serde_json::to_string_pretty(changeset)?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn load_changeset(path: &Path) -> Result<Changeset, Box<dyn std::error::Error>> {
    let contents = std::fs::read_to_string(path)?;
    let changeset: Changeset = serde_json::from_str(&contents)?;
    if changeset.version != 1 {
        return Err(format!(
            "unsupported changeset version: {} (expected 1)",
            changeset.version
        )
        .into());
    }
    Ok(changeset)
}

pub fn snapshot_data(
    sources: &HashMap<String, DataSource>,
    resolved_vars: &HashMap<String, serde_json::Value>,
) -> HashMap<String, DataSnapshot> {
    let mut snapshots = HashMap::new();
    for (name, source) in sources {
        let prefix = format!("data.{name}.");
        let values: HashMap<String, serde_json::Value> = resolved_vars
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(k, v)| (k[prefix.len()..].to_string(), v.clone()))
            .collect();

        snapshots.insert(
            name.clone(),
            DataSnapshot {
                source_type: source.source_type().to_string(),
                filters: source.filters.clone(),
                values,
            },
        );
    }
    snapshots
}

pub fn diff_data(
    old: &HashMap<String, DataSnapshot>,
    new: &HashMap<String, DataSnapshot>,
) -> Vec<DataChange> {
    let mut changes = Vec::new();

    for name in new.keys() {
        if !old.contains_key(name) {
            changes.push(DataChange::Added(name.clone()));
        }
    }

    for name in old.keys() {
        if !new.contains_key(name) {
            changes.push(DataChange::Removed(name.clone()));
        }
    }

    for (name, new_snap) in new {
        if let Some(old_snap) = old.get(name) {
            if old_snap.filters != new_snap.filters {
                changes.push(DataChange::FiltersChanged {
                    source: name.clone(),
                });
            }

            // Check for changed/added/removed values
            for (key, new_val) in &new_snap.values {
                match old_snap.values.get(key) {
                    Some(old_val) if old_val != new_val => {
                        changes.push(DataChange::Changed {
                            source: name.clone(),
                            key: key.clone(),
                            old_value: old_val.clone(),
                            new_value: new_val.clone(),
                        });
                    }
                    None => {
                        changes.push(DataChange::Changed {
                            source: name.clone(),
                            key: key.clone(),
                            old_value: serde_json::Value::Null,
                            new_value: new_val.clone(),
                        });
                    }
                    _ => {}
                }
            }

            for (key, old_val) in &old_snap.values {
                if !new_snap.values.contains_key(key) {
                    changes.push(DataChange::Changed {
                        source: name.clone(),
                        key: key.clone(),
                        old_value: old_val.clone(),
                        new_value: serde_json::Value::Null,
                    });
                }
            }
        }
    }

    changes
}

pub fn snapshot_resources_resolved(
    resources: &HashMap<String, config::Resource>,
    output_registry: &crate::reference::OutputRegistry,
) -> HashMap<String, ResourceSnapshot> {
    use crate::reference::Ref;

    let mut snapshots = HashMap::new();
    for (name, resource) in resources {
        let properties = match &resource.properties {
            Some(props) => {
                let json = toml_to_json(props);
                // Resolve any {{...}} refs in the serialized properties
                let text = json.to_string();
                match Ref::resolve_all(&text, output_registry) {
                    Ok(resolved) => serde_json::from_str(&resolved).unwrap_or(json),
                    Err(_) => json, // Fall back to unresolved if resolution fails
                }
            }
            None => serde_json::Value::Object(Default::default()),
        };
        snapshots.insert(
            name.clone(),
            ResourceSnapshot {
                resource_type: resource.resource_type().to_string(),
                status: ResourceStatus::Ready,
                properties,
                outputs: serde_json::Value::Object(Default::default()),
            },
        );
    }
    snapshots
}

fn diff_properties(
    old: &serde_json::Value,
    new: &serde_json::Value,
    schema: Option<&crate::schema::Schema>,
) -> Vec<PropertyChange> {
    let mut old_flat = Vec::new();
    flatten_json("", old, &mut old_flat);
    let old_map: HashMap<String, serde_json::Value> = old_flat.into_iter().collect();

    let mut new_flat = Vec::new();
    flatten_json("", new, &mut new_flat);
    let new_map: HashMap<String, serde_json::Value> = new_flat.into_iter().collect();

    let mut changes = Vec::new();

    for (field, new_value) in &new_map {
        match old_map.get(field) {
            None => {
                changes.push(PropertyChange::Added {
                    field: field.clone(),
                    new_value: new_value.clone(),
                });
            }
            Some(old_value) if old_value != new_value => {
                let force_new = schema.is_some_and(|s| s.is_force_new(field));
                changes.push(PropertyChange::Modified {
                    field: field.clone(),
                    old_value: old_value.clone(),
                    new_value: new_value.clone(),
                    force_new,
                });
            }
            _ => {}
        }
    }

    for (field, old_value) in &old_map {
        if !new_map.contains_key(field) {
            changes.push(PropertyChange::Removed {
                field: field.clone(),
                old_value: old_value.clone(),
            });
        }
    }

    changes
}

pub fn diff_resources(
    old: &HashMap<String, ResourceSnapshot>,
    new: &HashMap<String, ResourceSnapshot>,
    registry: &mut ProviderRegistry,
) -> Result<Vec<ResourceChange>, Box<dyn std::error::Error>> {
    let mut changes = Vec::new();

    for (name, new_snap) in new {
        match old.get(name) {
            None => {
                changes.push(ResourceChange::Create {
                    name: name.clone(),
                    resource_type: new_snap.resource_type.clone(),
                    properties: new_snap.properties.clone(),
                });
            }
            Some(old_snap) => {
                if old_snap.resource_type != new_snap.resource_type {
                    let schema = registry.resource_schema(&new_snap.resource_type)?;
                    let prop_changes =
                        diff_properties(&old_snap.properties, &new_snap.properties, schema);
                    changes.push(ResourceChange::Replace {
                        name: name.clone(),
                        resource_type: new_snap.resource_type.clone(),
                        changes: prop_changes,
                    });
                } else {
                    let schema = registry.resource_schema(&new_snap.resource_type)?;
                    let prop_changes =
                        diff_properties(&old_snap.properties, &new_snap.properties, schema);
                    if prop_changes.is_empty() {
                        changes.push(ResourceChange::Unchanged { name: name.clone() });
                    } else {
                        let has_force_new = prop_changes.iter().any(|c| {
                            matches!(
                                c,
                                PropertyChange::Modified {
                                    force_new: true,
                                    ..
                                }
                            )
                        });
                        if has_force_new {
                            changes.push(ResourceChange::Replace {
                                name: name.clone(),
                                resource_type: new_snap.resource_type.clone(),
                                changes: prop_changes,
                            });
                        } else {
                            changes.push(ResourceChange::Update {
                                name: name.clone(),
                                resource_type: new_snap.resource_type.clone(),
                                changes: prop_changes,
                            });
                        }
                    }
                }
            }
        }
    }

    for (name, old_snap) in old {
        if !new.contains_key(name) {
            changes.push(ResourceChange::Delete {
                name: name.clone(),
                resource_type: old_snap.resource_type.clone(),
            });
        }
    }

    Ok(changes)
}
