use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

use crate::schema::json_value_to_string;



/// Hook registry stores all hooks organized for easy access during deployment
#[derive(Debug, Default, Clone)]
pub struct HookRegistry {
    pub data_hooks: HashMap<String, Vec<Hook>>,      // data.<name> -> hooks
    pub resource_hooks: HashMap<String, Vec<Hook>>,  // resources.<name> -> hooks
}

impl HookRegistry {
    pub fn new() -> Self {
        Self {
            data_hooks: HashMap::new(),
            resource_hooks: HashMap::new(),
        }
    }
    
    /// Get hooks for a specific data source and event
    pub fn get_data_hooks(&self, data_name: &str, event: &str) -> Vec<&Hook> {
        self.data_hooks
            .get(data_name)
            .map(|hooks| {
                hooks.iter()
                    .filter(|h| h.event == event)
                    .collect()
            })
            .unwrap_or_default()
    }
    
    /// Get hooks for a specific resource and event
    pub fn get_resource_hooks(&self, resource_name: &str, event: &str) -> Vec<&Hook> {
        self.resource_hooks
            .get(resource_name)
            .map(|hooks| {
                hooks.iter()
                    .filter(|h| h.event == event)
                    .collect()
            })
            .unwrap_or_default()
    }
    
    /// Get all hooks for a specific data source
    pub fn get_all_data_hooks(&self, data_name: &str) -> Vec<&Hook> {
        self.data_hooks
            .get(data_name)
            .map(|hooks| hooks.iter().collect())
            .unwrap_or_default()
    }
    
    /// Get all hooks for a specific resource
    pub fn get_all_resource_hooks(&self, resource_name: &str) -> Vec<&Hook> {
        self.resource_hooks
            .get(resource_name)
            .map(|hooks| hooks.iter().collect())
            .unwrap_or_default()
    }
}

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
    pub resources: HashMap<String, Resource>,
    #[serde(skip)]  // Built during loading, not from TOML
    pub hook_registry: HookRegistry,
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

#[derive(Debug, Deserialize)]
pub struct Parameter {
    pub description: Option<String>,
    pub default: Option<toml::Value>,
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

#[derive(Deserialize)]
struct FirstPass {
    #[serde(default)]
    parameters: HashMap<String, Parameter>,
    #[serde(default)]
    data: HashMap<String, DataSource>,
}

pub fn extract_data_sources(
    raw: &str,
) -> Result<HashMap<String, DataSource>, Box<dyn std::error::Error>> {
    let first: FirstPass = toml::from_str(raw)?;
    // Validate all type formats early
    for (name, source) in &first.data {
        source
            .provider_and_type()
            .map_err(|e| format!("data.{name}: {e}"))?;
    }
    Ok(first.data)
}

pub fn load(
    raw: &str,
    overrides: &HashMap<String, String>,
    data_vars: &HashMap<String, serde_json::Value>,
) -> Result<Config, Box<dyn std::error::Error>> {
    // First pass: extract parameter defaults and data sources
    let first: FirstPass = toml::from_str(raw)?;
    let mut vars = HashMap::new();
    for (key, param) in &first.parameters {
        if let Some(ref val) = param.default {
            vars.insert(key.clone(), toml_value_to_string(val));
        }
    }

    // Data vars (data.<name>.<field>) - convert serde_json::Value to String
    for (k, v) in data_vars {
        vars.insert(k.clone(), json_value_to_string(v));
    }

    // CLI overrides win
    for (k, v) in overrides {
        vars.insert(k.clone(), v.clone());
    }

    // Interpolate placeholders (defer resource references)
    let interpolated = interpolate(raw, &vars, &["resources."])?;

    let mut config: Config = toml::from_str(&interpolated)?;
    
    // Build hook registry
    let data_sources = extract_data_sources(raw)?;
    config.hook_registry = build_hook_registry(&data_sources, &config.resources);
    
    Ok(config)
}

pub fn load_for_validation(
    raw: &str,
    overrides: &HashMap<String, String>,
) -> Result<Config, Box<dyn std::error::Error>> {
    let first: FirstPass = toml::from_str(raw)?;
    let mut vars = HashMap::new();
    for (key, param) in &first.parameters {
        if let Some(ref val) = param.default {
            vars.insert(key.clone(), toml_value_to_string(val));
        }
    }
    for (k, v) in overrides {
        vars.insert(k.clone(), v.clone());
    }
    let interpolated = interpolate(raw, &vars, &["resources.", "data."])?;
    let mut config: Config = toml::from_str(&interpolated)?;
    
    // Build hook registry for validation
    let data_sources = extract_data_sources(raw)?;
    config.hook_registry = build_hook_registry(&data_sources, &config.resources);
    
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

fn interpolate(
    text: &str,
    vars: &HashMap<String, String>,
    defer_prefixes: &[&str],
) -> Result<String, Box<dyn std::error::Error>> {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(start) = rest.find("{{") {
        result.push_str(&rest[..start]);
        let after_open = &rest[start + 2..];
        let end = after_open
            .find("}}")
            .ok_or_else(|| format!("unclosed '{{{{' at byte {start}"))?;
        let key = after_open[..end].trim();
        if defer_prefixes.iter().any(|p| key.starts_with(p)) {
            // Leave deferred references as-is
            result.push_str(&rest[start..start + 2 + end + 2]);
        } else {
            let value = vars
                .get(key)
                .ok_or_else(|| format!("unresolved variable: {key}"))?;
            result.push_str(value);
        }
        rest = &after_open[end + 2..];
    }
    result.push_str(rest);
    Ok(result)
}

pub fn extract_resource_refs(text: &str) -> Vec<(String, String)> {
    let mut refs = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let after_open = &rest[start + 2..];
        if let Some(end) = after_open.find("}}") {
            let key = after_open[..end].trim();
            if let Some(suffix) = key.strip_prefix("resources.")
                && let Some((name, field)) = suffix.split_once('.')
            {
                refs.push((name.to_string(), field.to_string()));
            }
            rest = &after_open[end + 2..];
        } else {
            break;
        }
    }
    refs
}

pub fn resolve_resource_refs(
    text: &str,
    resource_outputs: &HashMap<String, serde_json::Value>,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(start) = rest.find("{{") {
        result.push_str(&rest[..start]);
        let after_open = &rest[start + 2..];
        let end = after_open
            .find("}}")
            .ok_or_else(|| format!("unclosed '{{{{' at byte {start}"))?;
        let key = after_open[..end].trim();
        if let Some(suffix) = key.strip_prefix("resources.") {
            if let Some((name, field)) = suffix.split_once('.') {
                let outputs = resource_outputs
                    .get(name)
                    .ok_or_else(|| format!("unresolved resource reference: {key}"))?;
                let value = outputs
                    .get(field)
                    .ok_or_else(|| format!("resource '{name}' has no output '{field}'"))?;
                match value.as_str() {
                    Some(s) => result.push_str(s),
                    None => result.push_str(&value.to_string()),
                }
            } else {
                return Err(format!("invalid resource reference: {key}").into());
            }
        } else {
            // Not a resource ref, leave as-is (shouldn't happen after first pass)
            result.push_str(&rest[start..start + 2 + end + 2]);
        }
        rest = &after_open[end + 2..];
    }
    result.push_str(rest);
    Ok(result)
}

/// Build hook registry from configuration
pub fn build_hook_registry(
    data_sources: &HashMap<String, DataSource>,
    resources: &HashMap<String, Resource>,
) -> HookRegistry {
    let mut registry = HookRegistry::new();
    
    // Add data source hooks to registry
    for (name, source) in data_sources {
        if !source.hooks.is_empty() {
            registry.data_hooks.insert(name.clone(), source.hooks.clone());
        }
    }
    
    // Add resource hooks to registry
    for (name, resource) in resources {
        if !resource.hooks.is_empty() {
            registry.resource_hooks.insert(name.clone(), resource.hooks.clone());
        }
    }
    
    registry
}

/// Validate hook configurations
pub fn validate_hooks(
    hooks: &[Hook],
    base_path: &str,
    is_resource: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let valid_resource_events: &[&str] = &["before_create", "after_create", "before_update", "after_update", "before_delete", "after_delete"];
    let valid_data_events: &[&str] = &["before_read", "after_read"];
    
    for hook in hooks {
        // Validate event type
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
            ).into());
        }
        
        // Validate script file exists
        let script_path = Path::new(base_path).join(&hook.script);
        if !script_path.exists() {
            return Err(format!("Hook script not found: {}", script_path.display()).into());
        }
        
        // Validate output types
        for output in &hook.outputs {
            let valid_types = ["string", "integer", "float", "boolean", "array"];
            if !valid_types.contains(&output.r#type.as_str()) {
                return Err(format!(
                    "Invalid output type '{}' in hook. Valid types: {}",
                    output.r#type,
                    valid_types.join(", ")
                ).into());
            }
        }
    }
    
    Ok(())
}
