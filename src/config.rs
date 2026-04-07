use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

use crate::schema::FieldType;

#[derive(Debug, Deserialize, Clone)]
pub struct Hook {
    pub event: String,
    pub script: String,
    #[serde(default)]
    pub outputs: Vec<HookOutput>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct HookOutput {
    pub name: String,
    pub r#type: String,
}

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub parameters: HashMap<String, Parameter>,
    #[serde(default)]
    pub data: HashMap<String, DataSource>,
    #[serde(default)]
    pub resources: HashMap<String, Resource>,
    #[serde(skip)]
    pub overrides: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct Resource {
    #[serde(rename = "type")]
    resource_type: String,
    pub properties: Option<toml::Value>,
    #[serde(default)]
    pub hooks: Vec<Hook>,
}

impl Resource {
    pub fn resource_type(&self) -> &str {
        &self.resource_type
    }

    pub fn provider_and_type(&self) -> Result<(&str, &str), Box<dyn std::error::Error>> {
        split_provider_type(&self.resource_type)
    }
}

pub fn split_provider_type(s: &str) -> Result<(&str, &str), Box<dyn std::error::Error>> {
    s.split_once('.')
        .filter(|(provider, kind)| !provider.is_empty() && !kind.is_empty() && !kind.contains('.'))
        .ok_or_else(|| {
            format!("invalid type '{s}': expected 'provider.type' (e.g. 'upcloud.server')").into()
        })
}

#[derive(Debug, Deserialize, Clone)]
pub struct Parameter {
    pub description: Option<String>,
    pub default: Option<toml::Value>,
    #[serde(rename = "type")]
    param_type_explicit: Option<FieldType>,
    #[serde(default)]
    pub secret: bool,
    pub env: Option<String>,
}

impl Parameter {
    pub fn param_type(&self) -> FieldType {
        if let Some(ref t) = self.param_type_explicit {
            return t.clone();
        }
        if let Some(ref default) = self.default {
            return infer_field_type(default);
        }
        FieldType::String
    }
}

fn infer_field_type(value: &toml::Value) -> FieldType {
    match value {
        toml::Value::String(_) => FieldType::String,
        toml::Value::Integer(_) => FieldType::Integer,
        toml::Value::Float(_) => FieldType::Float,
        toml::Value::Boolean(_) => FieldType::Boolean,
        toml::Value::Array(_) => FieldType::Array,
        _ => FieldType::String,
    }
}

#[derive(Debug, Deserialize)]
pub struct DataSource {
    #[serde(rename = "type")]
    source_type: String,
    #[serde(default)]
    pub filters: HashMap<String, String>,
    #[serde(default)]
    pub hooks: Vec<Hook>,
}

impl DataSource {
    pub fn source_type(&self) -> &str {
        &self.source_type
    }

    pub fn provider_and_type(&self) -> Result<(&str, &str), Box<dyn std::error::Error>> {
        split_provider_type(&self.source_type)
    }
}

pub fn load(
    raw: &str,
    overrides: &HashMap<String, String>,
    config_dir: &Path,
) -> Result<Config, Box<dyn std::error::Error>> {
    let mut config: Config = toml::from_str(raw)?;
    config.overrides = overrides.clone();

    // Resolve all hook script paths relative to config directory
    resolve_hook_paths(&mut config, config_dir)?;

    Ok(config)
}

pub fn toml_value_to_string(val: &toml::Value) -> String {
    match val {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(n) => n.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Boolean(b) => b.to_string(),
        other => other.to_string(),
    }
}

/// Resolve all hook script paths to absolute paths within the config directory.
fn resolve_hook_paths(
    config: &mut Config,
    config_dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let canonical_dir = config_dir.canonicalize().map_err(|e| {
        format!(
            "failed to canonicalize config directory '{}': {e}",
            config_dir.display()
        )
    })?;

    let mut all_hooks: Vec<(&str, &mut Hook)> = Vec::new();
    for (name, source) in &mut config.data {
        for hook in &mut source.hooks {
            all_hooks.push((name, hook));
        }
    }
    for (name, resource) in &mut config.resources {
        for hook in &mut resource.hooks {
            all_hooks.push((name, hook));
        }
    }

    for (name, hook) in all_hooks {
        let script_path = config_dir.join(&hook.script);
        let canonical_script = script_path.canonicalize().map_err(|e| {
            format!(
                "{name}: hook script not found '{}': {e}",
                script_path.display()
            )
        })?;

        if !canonical_script.starts_with(&canonical_dir) {
            return Err(format!(
                "{name}: hook script '{}' escapes config directory '{}'",
                hook.script,
                config_dir.display()
            )
            .into());
        }

        hook.script = canonical_script.to_string_lossy().to_string();
    }

    Ok(())
}

/// Validate hook configurations (script paths must already be resolved).
pub fn validate_hooks(hooks: &[Hook], is_resource: bool) -> Result<(), Box<dyn std::error::Error>> {
    let valid_resource_events: &[&str] = &[
        "before_create",
        "after_create",
        "before_update",
        "after_update",
        "before_delete",
        "after_delete",
    ];
    let valid_data_events: &[&str] = &["before_read", "after_read"];

    for hook in hooks {
        let valid_events = if is_resource {
            valid_resource_events
        } else {
            valid_data_events
        };

        if !valid_events.contains(&hook.event.as_str()) {
            return Err(format!(
                "Invalid hook event '{}'. Valid events for this type: {}",
                hook.event,
                valid_events.join(", ")
            )
            .into());
        }

        // Script path is already resolved and validated by resolve_hook_paths
        let script_path = Path::new(&hook.script);
        if !script_path.exists() {
            return Err(format!("Hook script not found: {}", script_path.display()).into());
        }

        for output in &hook.outputs {
            let valid_types = ["string", "integer", "float", "boolean", "array"];
            if !valid_types.contains(&output.r#type.as_str()) {
                return Err(format!(
                    "Invalid output type '{}' in hook. Valid types: {}",
                    output.r#type,
                    valid_types.join(", ")
                )
                .into());
            }
        }
    }

    Ok(())
}
