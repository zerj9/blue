use std::collections::HashMap;

use serde::Deserialize;
use serde_json::Value;

// --- Resource config (the -f file) ---

#[derive(Debug, Deserialize)]
pub struct ResourceConfig {
    #[serde(default)]
    pub encryption: Option<EncryptionConfig>,
    #[serde(default)]
    pub parameters: HashMap<String, ParameterConfig>,
    #[serde(default)]
    pub data: HashMap<String, DataSourceConfig>,
    #[serde(default)]
    pub resources: HashMap<String, ResourceDef>,
}

#[derive(Debug, Deserialize)]
pub struct EncryptionConfig {
    pub recipients: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ParameterConfig {
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub default: Option<Value>,
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub env: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DataSourceConfig {
    #[serde(rename = "type")]
    pub data_type: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(flatten)]
    pub config: HashMap<String, Value>,
}

#[derive(Debug, Deserialize)]
pub struct ResourceDef {
    #[serde(rename = "type")]
    pub resource_type: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub inputs: Option<HashMap<String, Value>>,
}

// --- Provider config (providers.toml) ---

#[derive(Debug)]
pub struct ProviderFile {
    pub data: HashMap<String, DataSourceConfig>,
    pub providers: HashMap<String, ProviderDef>,
}

#[derive(Debug, Deserialize)]
pub struct ProviderDef {
    #[serde(rename = "type")]
    pub provider_type: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(flatten)]
    pub config: HashMap<String, Value>,
}

pub fn parse_resource_config(toml_str: &str) -> Result<ResourceConfig, String> {
    toml::from_str(toml_str).map_err(|e| format!("Failed to parse resource config: {e}"))
}

pub fn parse_provider_config(toml_str: &str) -> Result<ProviderFile, String> {
    let table: toml::Table =
        toml::from_str(toml_str).map_err(|e| format!("Failed to parse provider config: {e}"))?;

    let data = match table.get("data") {
        Some(v) => {
            let data_map: HashMap<String, DataSourceConfig> = v
                .clone()
                .try_into()
                .map_err(|e| format!("Failed to parse data sources in provider config: {e}"))?;
            data_map
        }
        None => HashMap::new(),
    };

    let mut providers = HashMap::new();
    for (key, value) in &table {
        if key == "data" {
            continue;
        }
        let provider_def: ProviderDef = value
            .clone()
            .try_into()
            .map_err(|e| format!("Failed to parse provider '{key}': {e}"))?;
        providers.insert(key.clone(), provider_def);
    }

    Ok(ProviderFile { data, providers })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_resource_config_full() {
        let toml = r#"
[encryption]
recipients = ["age1abc"]

[parameters.github_token]
description = "GitHub PAT"
secret = true
env = "GITHUB_TOKEN"

[parameters.region]
default = "uk-lon1"

[data.ubuntu]
type = "upcloud.storage"
filters = { type = "template", title = "Ubuntu Server 24.04 LTS" }

[data.vault_creds]
type = "blue.script"
script = "scripts/fetch_creds.js"
inputs = { path = "secret/upcloud" }

[resources.web-01]
type = "upcloud.server"
provider = "upcloud-us"

[resources.web-01.inputs]
hostname = "web-01"
zone = "uk-lon1"
storage = "{{ data.ubuntu.uuid }}"

[resources.random_id]
type = "blue.script"

[resources.random_id.inputs]
script = "scripts/generate_id.js"
triggers_replace = { name = "test" }
"#;

        let config = parse_resource_config(toml).unwrap();

        assert_eq!(config.encryption.unwrap().recipients, vec!["age1abc"]);
        assert_eq!(config.parameters.len(), 2);
        assert!(config.parameters["github_token"].secret);
        assert_eq!(
            config.parameters["region"].default,
            Some(Value::String("uk-lon1".into()))
        );
        assert_eq!(config.data.len(), 2);
        assert_eq!(config.data["ubuntu"].data_type, "upcloud.storage");
        assert_eq!(
            config.data["vault_creds"].config["script"],
            Value::String("scripts/fetch_creds.js".into())
        );
        assert_eq!(config.resources.len(), 2);
        assert_eq!(config.resources["web-01"].resource_type, "upcloud.server");
        assert_eq!(
            config.resources["web-01"].provider.as_deref(),
            Some("upcloud-us")
        );
        assert!(config.resources["web-01"].inputs.is_some());
        assert_eq!(
            config.resources["random_id"].inputs.as_ref().unwrap()["script"],
            Value::String("scripts/generate_id.js".into())
        );
    }

    #[test]
    fn parse_provider_config_basic() {
        let toml = r#"
[upcloud]
type = "upcloud"
username_env = "UPCLOUD_USER"
password_env = "UPCLOUD_PASS"

[upcloud-us]
type = "upcloud"
username_env = "UPCLOUD_US_USER"
password_env = "UPCLOUD_US_PASS"
"#;

        let config = parse_provider_config(toml).unwrap();

        assert_eq!(config.providers.len(), 2);
        assert_eq!(config.providers["upcloud"].provider_type, "upcloud");
        assert_eq!(
            config.providers["upcloud"].config["username_env"],
            Value::String("UPCLOUD_USER".into())
        );
    }

    #[test]
    fn parse_provider_config_with_script_data_source() {
        let toml = r#"
[data.vault_creds]
type = "blue.script"
script = "scripts/fetch_vault_creds.js"
inputs = { path = "secret/upcloud" }

[upcloud]
type = "upcloud"
username = "{{ data.vault_creds.username }}"
password = "{{ data.vault_creds.password }}"
"#;

        let config = parse_provider_config(toml).unwrap();

        assert_eq!(config.data.len(), 1);
        assert_eq!(config.data["vault_creds"].data_type, "blue.script");
        assert_eq!(config.providers.len(), 1);
        assert_eq!(
            config.providers["upcloud"].config["username"],
            Value::String("{{ data.vault_creds.username }}".into())
        );
    }
}
