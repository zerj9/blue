use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

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
}

pub fn load(
    raw: &str,
    overrides: &HashMap<String, String>,
    config_dir: &Path,
) -> Result<Config, Box<dyn std::error::Error>> {
    // First pass: extract parameter defaults
    let first: FirstPass = toml::from_str(raw)?;
    let mut vars = HashMap::new();
    for (key, param) in &first.parameters {
        if let Some(ref val) = param.default {
            vars.insert(key.clone(), toml_value_to_string(val));
        }
    }

    // CLI overrides win
    for (k, v) in overrides {
        vars.insert(k.clone(), v.clone());
    }

    // Interpolate only parameters/overrides; data.* and resources.* refs are deferred
    let interpolated = interpolate(raw, &vars)?;

    let mut config: Config = toml::from_str(&interpolated)?;

    // Resolve all hook script paths relative to config directory
    resolve_hook_paths(&mut config, config_dir)?;

    Ok(config)
}

pub fn load_for_validation(
    raw: &str,
    overrides: &HashMap<String, String>,
    config_dir: &Path,
) -> Result<Config, Box<dyn std::error::Error>> {
    load(raw, overrides, config_dir)
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
) -> Result<String, Box<dyn std::error::Error>> {
    use crate::reference::Ref;

    let mut result = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(start) = rest.find("{{") {
        result.push_str(&rest[..start]);
        let after_open = &rest[start + 2..];
        let end = after_open
            .find("}}")
            .ok_or_else(|| format!("unclosed '{{{{' at byte {start}"))?;
        let key = after_open[..end].trim();
        if Ref::parse(key).is_some() {
            // data/resource/hook ref — leave as-is for graph-driven resolution
            result.push_str(&rest[start..start + 2 + end + 2]);
        } else {
            // Parameter or override — resolve now
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
