use std::collections::HashMap;

use serde::Deserialize;
use serde_json::Value as JsonValue;

use crate::types::{FieldDef, FieldType, OutputDef, RetryConfig, Schema, TimeoutConfig};

#[derive(Deserialize)]
struct SchemaFile {
    #[serde(default)]
    inputs: HashMap<String, InputToml>,
    #[serde(default)]
    outputs: HashMap<String, OutputToml>,
    #[serde(default)]
    retry: Option<RetryConfig>,
    #[serde(default)]
    timeout: Option<TimeoutConfig>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ItemsDef {
    /// Simple array items: `items = { type = "string" }`
    Single(ItemType),
    /// Nested object items: `[inputs.rules.items.direction]`
    Map(HashMap<String, InputToml>),
}

#[derive(Deserialize)]
struct ItemType {
    #[serde(rename = "type")]
    field_type: FieldType,
}

#[derive(Deserialize)]
struct InputToml {
    #[serde(rename = "type")]
    field_type: FieldType,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    force_new: bool,
    #[serde(default)]
    requires_stop: bool,
    #[serde(default)]
    default: Option<JsonValue>,
    #[serde(default)]
    items: Option<ItemsDef>,
    #[serde(default = "default_true")]
    ordered: bool,
}

#[derive(Deserialize)]
struct OutputToml {
    #[serde(rename = "type")]
    field_type: FieldType,
    #[serde(default)]
    secret: bool,
}

fn default_true() -> bool {
    true
}

fn input_toml_to_field_def(name: &str, input: &InputToml) -> FieldDef {
    let items = match &input.items {
        None => vec![],
        Some(ItemsDef::Single(item_type)) => vec![FieldDef {
            path: String::new(),
            field_type: item_type.field_type.clone(),
            required: false,
            force_new: false,
            requires_stop: false,
            default: None,
            items: vec![],
            ordered: true,
        }],
        Some(ItemsDef::Map(map)) => map
            .iter()
            .map(|(child_name, child)| input_toml_to_field_def(child_name, child))
            .collect(),
    };

    FieldDef {
        path: name.to_string(),
        field_type: input.field_type.clone(),
        required: input.required,
        force_new: input.force_new,
        requires_stop: input.requires_stop,
        default: input.default.clone(),
        items,
        ordered: input.ordered,
    }
}

pub fn parse_schema(toml_str: &str) -> Result<Schema, String> {
    let schema_file: SchemaFile =
        toml::from_str(toml_str).map_err(|e| format!("Failed to parse schema: {e}"))?;

    let inputs = schema_file
        .inputs
        .iter()
        .map(|(name, input)| input_toml_to_field_def(name, input))
        .collect();

    let outputs = schema_file
        .outputs
        .iter()
        .map(|(name, output)| OutputDef {
            path: name.to_string(),
            field_type: output.field_type.clone(),
            secret: output.secret,
        })
        .collect();

    Ok(Schema {
        inputs,
        outputs,
        retry: schema_file.retry,
        timeout: schema_file.timeout,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_schema() {
        let toml = r#"
[inputs.hostname]
type = "string"
required = true
force_new = true

[inputs.zone]
type = "string"
required = true
force_new = true

[inputs.plan]
type = "string"
required = true
requires_stop = true

[inputs.tags]
type = "array"
ordered = false
items = { type = "string" }

[outputs.uuid]
type = "string"

[outputs.state]
type = "string"

[outputs.password]
type = "string"
secret = true

[retry]
max_attempts = 3
interval_seconds = 5
"#;

        let schema = parse_schema(toml).unwrap();

        assert_eq!(schema.inputs.len(), 4);
        assert_eq!(schema.outputs.len(), 3);

        let hostname = schema.inputs.iter().find(|f| f.path == "hostname").unwrap();
        assert!(hostname.required);
        assert!(hostname.force_new);
        assert!(!hostname.requires_stop);

        let plan = schema.inputs.iter().find(|f| f.path == "plan").unwrap();
        assert!(plan.requires_stop);
        assert!(!plan.force_new);

        let tags = schema.inputs.iter().find(|f| f.path == "tags").unwrap();
        assert!(!tags.ordered);

        let password = schema.outputs.iter().find(|o| o.path == "password").unwrap();
        assert!(password.secret);

        let uuid = schema.outputs.iter().find(|o| o.path == "uuid").unwrap();
        assert!(!uuid.secret);

        let retry = schema.retry.unwrap();
        assert_eq!(retry.max_attempts, 3);
        assert_eq!(retry.interval_seconds, 5);
    }

    #[test]
    fn parse_schema_no_retry() {
        let toml = r#"
[inputs.name]
type = "string"
required = true

[outputs.id]
type = "string"
"#;

        let schema = parse_schema(toml).unwrap();
        assert!(schema.retry.is_none());
    }

    #[test]
    fn parse_schema_nested_array_items() {
        let toml = r#"
[inputs.firewall_rules]
type = "array"

[inputs.firewall_rules.items.direction]
type = "string"
required = true

[inputs.firewall_rules.items.action]
type = "string"
required = true

[inputs.firewall_rules.items.source]
type = "string"

[outputs.uuid]
type = "string"
"#;

        let schema = parse_schema(toml).unwrap();

        let rules = schema.inputs.iter().find(|f| f.path == "firewall_rules").unwrap();
        assert!(rules.ordered);
        assert_eq!(rules.items.len(), 3);

        let direction = rules.items.iter().find(|f| f.path == "direction").unwrap();
        assert!(direction.required);
    }
}
