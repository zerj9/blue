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
    #[serde(default)]
    fields: Option<HashMap<String, InputToml>>,
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
            fields: vec![],
            ordered: true,
        }],
        Some(ItemsDef::Map(map)) => map
            .iter()
            .map(|(child_name, child)| input_toml_to_field_def(child_name, child))
            .collect(),
    };

    let fields = match &input.fields {
        None => vec![],
        Some(map) => map
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
        fields,
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

// --- Validation ---

/// Validate a resolved input value against an input schema.
///
/// Returns the first error encountered. The error path is dot-qualified
/// (e.g. `backup_rule.interval` or `firewall_rules.0.direction`).
///
/// Behavior:
/// - missing required fields error
/// - type mismatches error (string/number/boolean/array/object)
/// - unknown fields are rejected when the surrounding schema declares its fields
/// - objects with no `fields` declared are permissive (any keys accepted)
/// - arrays with no `items` declared are permissive (any element shape accepted)
pub fn validate_inputs(schema: &[FieldDef], value: &JsonValue) -> Result<(), String> {
    validate_object_against_fields(value, schema, "")
}

fn validate_object_against_fields(
    value: &JsonValue,
    fields: &[FieldDef],
    path: &str,
) -> Result<(), String> {
    let obj = value.as_object().ok_or_else(|| {
        format!(
            "expected object at {}, got {}",
            display_path(path),
            describe_type(value)
        )
    })?;

    for field in fields {
        if field.required && !obj.contains_key(&field.path) {
            return Err(format!(
                "missing required field '{}'",
                qualify(path, &field.path)
            ));
        }
    }

    for (key, val) in obj {
        match fields.iter().find(|f| f.path == *key) {
            None => return Err(format!("unknown field '{}'", qualify(path, key))),
            Some(def) => validate_field(val, def, &qualify(path, key))?,
        }
    }

    Ok(())
}

fn validate_field(value: &JsonValue, field: &FieldDef, path: &str) -> Result<(), String> {
    if !type_matches(value, &field.field_type) {
        return Err(format!(
            "field '{}' expected {}, got {}",
            path,
            field_type_name(&field.field_type),
            describe_type(value)
        ));
    }

    match &field.field_type {
        FieldType::Object if !field.fields.is_empty() => {
            validate_object_against_fields(value, &field.fields, path)?;
        }
        FieldType::Array if !field.items.is_empty() => {
            let arr = value.as_array().expect("type checked above");
            for (i, item) in arr.iter().enumerate() {
                let item_path = format!("{path}.{i}");
                if field.items.len() == 1 && field.items[0].path.is_empty() {
                    validate_field(item, &field.items[0], &item_path)?;
                } else {
                    validate_object_against_fields(item, &field.items, &item_path)?;
                }
            }
        }
        _ => {}
    }

    Ok(())
}

fn type_matches(value: &JsonValue, expected: &FieldType) -> bool {
    match expected {
        FieldType::String => value.is_string(),
        FieldType::Number => value.is_number(),
        FieldType::Boolean => value.is_boolean(),
        FieldType::Array => value.is_array(),
        FieldType::Object => value.is_object(),
    }
}

fn describe_type(value: &JsonValue) -> &'static str {
    match value {
        JsonValue::String(_) => "string",
        JsonValue::Number(_) => "number",
        JsonValue::Bool(_) => "boolean",
        JsonValue::Array(_) => "array",
        JsonValue::Object(_) => "object",
        JsonValue::Null => "null",
    }
}

fn field_type_name(t: &FieldType) -> &'static str {
    match t {
        FieldType::String => "string",
        FieldType::Number => "number",
        FieldType::Boolean => "boolean",
        FieldType::Array => "array",
        FieldType::Object => "object",
    }
}

fn qualify(parent: &str, child: &str) -> String {
    if parent.is_empty() {
        child.to_string()
    } else {
        format!("{parent}.{child}")
    }
}

fn display_path(path: &str) -> String {
    if path.is_empty() {
        "<root>".to_string()
    } else {
        format!("'{path}'")
    }
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

    #[test]
    fn parse_schema_nested_object_fields() {
        let toml = r#"
[inputs.backup_rule]
type = "object"

[inputs.backup_rule.fields.interval]
type = "string"
required = true

[inputs.backup_rule.fields.retention]
type = "number"

[outputs.uuid]
type = "string"
"#;

        let schema = parse_schema(toml).unwrap();

        let backup = schema.inputs.iter().find(|f| f.path == "backup_rule").unwrap();
        assert!(matches!(backup.field_type, FieldType::Object));
        assert!(backup.items.is_empty());
        assert_eq!(backup.fields.len(), 2);

        let interval = backup.fields.iter().find(|f| f.path == "interval").unwrap();
        assert!(matches!(interval.field_type, FieldType::String));
        assert!(interval.required);

        let retention = backup.fields.iter().find(|f| f.path == "retention").unwrap();
        assert!(matches!(retention.field_type, FieldType::Number));
        assert!(!retention.required);
    }

    #[test]
    fn parse_schema_object_without_fields_is_permissive() {
        // An object input with no `fields` declared should parse with an empty
        // `fields` vec — preserves the existing "untyped object" semantics
        // used by e.g. blue.script's [inputs.inputs].
        let toml = r#"
[inputs.payload]
type = "object"
"#;

        let schema = parse_schema(toml).unwrap();
        let payload = schema.inputs.iter().find(|f| f.path == "payload").unwrap();
        assert!(payload.fields.is_empty());
    }

    // --- Validation tests ---

    use serde_json::json;

    fn schema_for(toml: &str) -> Vec<FieldDef> {
        parse_schema(toml).unwrap().inputs
    }

    #[test]
    fn validate_passes_on_well_formed_inputs() {
        let inputs = schema_for(
            r#"
[inputs.script]
type = "string"
required = true

[inputs.triggers_replace]
type = "object"
"#,
        );
        let value = json!({"script": "x.js", "triggers_replace": {"any": "thing"}});
        validate_inputs(&inputs, &value).unwrap();
    }

    #[test]
    fn validate_errors_on_missing_required_field() {
        let inputs = schema_for(
            r#"
[inputs.script]
type = "string"
required = true
"#,
        );
        let value = json!({});
        let err = validate_inputs(&inputs, &value).unwrap_err();
        assert!(err.contains("missing required field 'script'"), "got: {err}");
    }

    #[test]
    fn validate_errors_on_unknown_top_level_field() {
        let inputs = schema_for(
            r#"
[inputs.script]
type = "string"
required = true
"#,
        );
        let value = json!({"script": "x.js", "hsotname": "typo"});
        let err = validate_inputs(&inputs, &value).unwrap_err();
        assert!(err.contains("unknown field 'hsotname'"), "got: {err}");
    }

    #[test]
    fn validate_errors_on_type_mismatch_with_path() {
        let inputs = schema_for(
            r#"
[inputs.size]
type = "number"
"#,
        );
        let value = json!({"size": "ten"});
        let err = validate_inputs(&inputs, &value).unwrap_err();
        assert!(err.contains("'size'"), "got: {err}");
        assert!(err.contains("expected number"), "got: {err}");
        assert!(err.contains("got string"), "got: {err}");
    }

    #[test]
    fn validate_recurses_into_declared_object_fields() {
        let inputs = schema_for(
            r#"
[inputs.backup_rule]
type = "object"

[inputs.backup_rule.fields.interval]
type = "string"
required = true

[inputs.backup_rule.fields.retention]
type = "number"
"#,
        );

        // Happy path
        let ok = json!({"backup_rule": {"interval": "daily", "retention": 14}});
        validate_inputs(&inputs, &ok).unwrap();

        // Unknown nested key
        let bad = json!({"backup_rule": {"interval": "daily", "freqency": "x"}});
        let err = validate_inputs(&inputs, &bad).unwrap_err();
        assert!(
            err.contains("unknown field 'backup_rule.freqency'"),
            "got: {err}"
        );

        // Wrong nested type
        let wrong = json!({"backup_rule": {"interval": "daily", "retention": "two weeks"}});
        let err = validate_inputs(&inputs, &wrong).unwrap_err();
        assert!(err.contains("'backup_rule.retention'"), "got: {err}");
        assert!(err.contains("expected number"), "got: {err}");

        // Missing nested required
        let missing = json!({"backup_rule": {"retention": 14}});
        let err = validate_inputs(&inputs, &missing).unwrap_err();
        assert!(
            err.contains("missing required field 'backup_rule.interval'"),
            "got: {err}"
        );
    }

    #[test]
    fn validate_object_without_declared_fields_is_permissive() {
        let inputs = schema_for(
            r#"
[inputs.triggers_replace]
type = "object"
"#,
        );
        let value = json!({"triggers_replace": {"any": "key", "is": "fine", "nested": {"too": true}}});
        validate_inputs(&inputs, &value).unwrap();
    }

    #[test]
    fn validate_recurses_into_array_of_objects_with_indexed_path() {
        let inputs = schema_for(
            r#"
[inputs.firewall_rules]
type = "array"

[inputs.firewall_rules.items.direction]
type = "string"
required = true

[inputs.firewall_rules.items.action]
type = "string"
required = true
"#,
        );

        // Happy path
        let ok = json!({
            "firewall_rules": [
                {"direction": "in", "action": "accept"},
                {"direction": "out", "action": "drop"},
            ]
        });
        validate_inputs(&inputs, &ok).unwrap();

        // Missing required in second element
        let bad = json!({
            "firewall_rules": [
                {"direction": "in", "action": "accept"},
                {"direction": "out"},
            ]
        });
        let err = validate_inputs(&inputs, &bad).unwrap_err();
        assert!(
            err.contains("missing required field 'firewall_rules.1.action'"),
            "got: {err}"
        );
    }

    #[test]
    fn validate_recurses_into_array_of_primitives() {
        let inputs = schema_for(
            r#"
[inputs.tags]
type = "array"
items = { type = "string" }
"#,
        );

        validate_inputs(&inputs, &json!({"tags": ["a", "b", "c"]})).unwrap();

        let err = validate_inputs(&inputs, &json!({"tags": ["a", 42, "c"]})).unwrap_err();
        assert!(err.contains("'tags.1'"), "got: {err}");
        assert!(err.contains("expected string"), "got: {err}");
    }

    #[test]
    fn validate_array_without_declared_items_is_permissive() {
        let inputs = schema_for(
            r#"
[inputs.anything]
type = "array"
"#,
        );
        validate_inputs(&inputs, &json!({"anything": [1, "two", {"three": true}]})).unwrap();
    }
}
