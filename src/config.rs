use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub parameters: HashMap<String, Parameter>,
    #[serde(default)]
    pub resources: HashMap<String, Resource>,
}

#[derive(Debug, Deserialize)]
pub struct Resource {
    #[serde(rename = "type")]
    resource_type: String,
    pub properties: Option<toml::Value>,
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
    data_vars: &HashMap<String, String>,
) -> Result<Config, Box<dyn std::error::Error>> {
    // First pass: extract parameter defaults
    let first: FirstPass = toml::from_str(raw)?;
    let mut vars = HashMap::new();
    for (key, param) in &first.parameters {
        if let Some(ref val) = param.default {
            vars.insert(key.clone(), toml_value_to_string(val));
        }
    }

    // Data vars (data.<name>.<field>)
    for (k, v) in data_vars {
        vars.insert(k.clone(), v.clone());
    }

    // CLI overrides win
    for (k, v) in overrides {
        vars.insert(k.clone(), v.clone());
    }

    // Interpolate placeholders (defer resource references)
    let interpolated = interpolate(raw, &vars, &["resources."])?;

    let config: Config = toml::from_str(&interpolated)?;
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
    let config: Config = toml::from_str(&interpolated)?;
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
            if let Some(suffix) = key.strip_prefix("resources.") {
                if let Some((name, field)) = suffix.split_once('.') {
                    refs.push((name.to_string(), field.to_string()));
                }
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
