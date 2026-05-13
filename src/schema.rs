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

// --- Defaults and validation ---

/// Apply schema defaults to a resolved input value.
///
/// For each top-level field in `schema`:
/// - If the field is missing from `value` and has a `default`, the default
///   is inserted.
/// - If the field is present and is an object with declared `fields`,
///   recurse into the child schema so nested defaults can fill in.
/// - If the field is present and is an array whose `items` declare nested
///   fields (i.e. an "array of objects" — not scalar items), recurse into
///   each element so per-item defaults fill in. Arrays of scalars are left
///   alone since they have no per-item fields to default.
///
/// Defaults are substitution, not synthesis: an absent intermediate object
/// or array stays absent — its children's defaults don't materialize a
/// parent. This function is idempotent.
pub fn apply_defaults(schema: &[FieldDef], value: JsonValue) -> JsonValue {
    let mut obj = value.as_object().cloned().unwrap_or_default();
    for field in schema {
        match obj.get(&field.path) {
            None => {
                if let Some(default) = &field.default {
                    obj.insert(field.path.clone(), default.clone());
                }
            }
            Some(existing) => {
                if matches!(field.field_type, FieldType::Object) && !field.fields.is_empty() {
                    let recursed = apply_defaults(&field.fields, existing.clone());
                    obj.insert(field.path.clone(), recursed);
                } else if matches!(field.field_type, FieldType::Array)
                    && !field.items.is_empty()
                    && !(field.items.len() == 1 && field.items[0].path.is_empty())
                {
                    if let Some(arr) = existing.as_array() {
                        let recursed: Vec<JsonValue> = arr
                            .iter()
                            .map(|item| apply_defaults(&field.items, item.clone()))
                            .collect();
                        obj.insert(field.path.clone(), JsonValue::Array(recursed));
                    }
                }
            }
        }
    }
    JsonValue::Object(obj)
}

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

// === Validation against a Resolvable (validate-with-holes) ===
//
// Same shape as `validate_inputs` but works on a `Resolvable`. Concrete
// (`Known`) leaves are validated by the existing Value-based validator.
// Pending (`Unknown`) leaves are checked against their `expected_type`
// (computed by the resolver from the destination schema) — type
// mismatches between upstream output type and downstream input type are
// caught at plan time, even when the value isn't yet known.
//
// Required-field checks treat `Unknown` as "present" — the value will
// arrive at deploy time. Compositional `Object`/`Array` variants recurse.

use crate::resolvable::Resolvable;

pub fn validate_resolvable(schema: &[FieldDef], value: &Resolvable) -> Result<(), String> {
    validate_resolvable_object_against_fields(value, schema, "")
}

fn validate_resolvable_object_against_fields(
    value: &Resolvable,
    fields: &[FieldDef],
    path: &str,
) -> Result<(), String> {
    match value {
        // Concrete subtree — fall through to the existing Value-based
        // validator. All the existing rules (required, unknown keys,
        // nested types) apply unchanged.
        Resolvable::Known(v) => validate_object_against_fields(v, fields, path),

        Resolvable::Object(map) => {
            // Required-field check: Unknown counts as present (the value
            // will be filled in at deploy time).
            for field in fields {
                if field.required && !map.contains_key(&field.path) {
                    return Err(format!(
                        "missing required field '{}'",
                        qualify(path, &field.path)
                    ));
                }
            }
            for (key, val) in map {
                match fields.iter().find(|f| f.path == *key) {
                    None => return Err(format!("unknown field '{}'", qualify(path, key))),
                    Some(def) => validate_resolvable_field(val, def, &qualify(path, key))?,
                }
            }
            Ok(())
        }

        // The whole object is a pending ref. Type-check expected_type
        // against Object; we can't enumerate keys to check required
        // fields, so we trust the destination schema and let deploy-time
        // re-validation catch any structural problems.
        Resolvable::Unknown { expected_type, .. } => match expected_type {
            None | Some(FieldType::Object) => Ok(()),
            Some(other) => Err(format!(
                "expected object at {}, got pending value of type {}",
                display_path(path),
                field_type_name(other)
            )),
        },

        Resolvable::Array(_) => Err(format!(
            "expected object at {}, got array",
            display_path(path)
        )),
    }
}

fn validate_resolvable_field(
    value: &Resolvable,
    field: &FieldDef,
    path: &str,
) -> Result<(), String> {
    match value {
        Resolvable::Known(v) => validate_field(v, field, path),

        Resolvable::Unknown { expected_type, .. } => {
            // Permissive context (None) accepts any destination type.
            // Otherwise check the upstream's declared output type matches
            // the destination's declared input type.
            match expected_type {
                None => Ok(()),
                Some(t) if field_types_compatible(t, &field.field_type) => Ok(()),
                Some(t) => Err(format!(
                    "field '{}' expected {}, got pending value of type {}",
                    path,
                    field_type_name(&field.field_type),
                    field_type_name(t),
                )),
            }
        }

        Resolvable::Object(_) => {
            if !matches!(field.field_type, FieldType::Object) {
                return Err(format!(
                    "field '{}' expected {}, got object",
                    path,
                    field_type_name(&field.field_type)
                ));
            }
            if !field.fields.is_empty() {
                validate_resolvable_object_against_fields(value, &field.fields, path)
            } else {
                // Permissive object — nothing further to enforce.
                Ok(())
            }
        }

        Resolvable::Array(arr) => {
            if !matches!(field.field_type, FieldType::Array) {
                return Err(format!(
                    "field '{}' expected {}, got array",
                    path,
                    field_type_name(&field.field_type)
                ));
            }
            if field.items.is_empty() {
                return Ok(());
            }
            for (i, item) in arr.iter().enumerate() {
                let item_path = format!("{path}.{i}");
                if field.items.len() == 1 && field.items[0].path.is_empty() {
                    validate_resolvable_field(item, &field.items[0], &item_path)?;
                } else {
                    validate_resolvable_object_against_fields(item, &field.items, &item_path)?;
                }
            }
            Ok(())
        }
    }
}

/// Whether an upstream output type is compatible with a downstream input
/// type. v1: exact equality. Future could add coercion (e.g. number → string).
fn field_types_compatible(upstream: &FieldType, downstream: &FieldType) -> bool {
    upstream == downstream
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

        let password = schema
            .outputs
            .iter()
            .find(|o| o.path == "password")
            .unwrap();
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

        let rules = schema
            .inputs
            .iter()
            .find(|f| f.path == "firewall_rules")
            .unwrap();
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

        let backup = schema
            .inputs
            .iter()
            .find(|f| f.path == "backup_rule")
            .unwrap();
        assert!(matches!(backup.field_type, FieldType::Object));
        assert!(backup.items.is_empty());
        assert_eq!(backup.fields.len(), 2);

        let interval = backup.fields.iter().find(|f| f.path == "interval").unwrap();
        assert!(matches!(interval.field_type, FieldType::String));
        assert!(interval.required);

        let retention = backup
            .fields
            .iter()
            .find(|f| f.path == "retention")
            .unwrap();
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
        assert!(
            err.contains("missing required field 'script'"),
            "got: {err}"
        );
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
        let value =
            json!({"triggers_replace": {"any": "key", "is": "fine", "nested": {"too": true}}});
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

    // --- Defaults tests ---

    #[test]
    fn apply_defaults_fills_missing_top_level_field() {
        let inputs = schema_for(
            r#"
[inputs.tier]
type = "string"
default = "hdd"

[inputs.title]
type = "string"
required = true
"#,
        );
        let result = apply_defaults(&inputs, json!({"title": "data-disk"}));
        assert_eq!(result["title"], "data-disk");
        assert_eq!(result["tier"], "hdd");
    }

    #[test]
    fn apply_defaults_does_not_overwrite_existing_value() {
        let inputs = schema_for(
            r#"
[inputs.tier]
type = "string"
default = "hdd"
"#,
        );
        let result = apply_defaults(&inputs, json!({"tier": "maxiops"}));
        assert_eq!(result["tier"], "maxiops");
    }

    #[test]
    fn apply_defaults_is_idempotent() {
        let inputs = schema_for(
            r#"
[inputs.tier]
type = "string"
default = "hdd"
"#,
        );
        let once = apply_defaults(&inputs, json!({}));
        let twice = apply_defaults(&inputs, once.clone());
        assert_eq!(once, twice);
    }

    #[test]
    fn apply_defaults_recurses_into_present_objects() {
        let inputs = schema_for(
            r#"
[inputs.backup_rule]
type = "object"

[inputs.backup_rule.fields.interval]
type = "string"

[inputs.backup_rule.fields.retention]
type = "number"
default = 30
"#,
        );
        // Parent present, retention missing — default fills in
        let result = apply_defaults(&inputs, json!({"backup_rule": {"interval": "daily"}}));
        assert_eq!(result["backup_rule"]["interval"], "daily");
        assert_eq!(result["backup_rule"]["retention"], 30);
    }

    #[test]
    fn apply_defaults_does_not_synthesize_missing_object_parent() {
        let inputs = schema_for(
            r#"
[inputs.backup_rule]
type = "object"

[inputs.backup_rule.fields.retention]
type = "number"
default = 30
"#,
        );
        // Parent absent and has no default — don't materialize empty object
        let result = apply_defaults(&inputs, json!({}));
        assert!(result.as_object().unwrap().is_empty());
    }

    #[test]
    fn apply_defaults_recurses_into_array_items() {
        let inputs = schema_for(
            r#"
[inputs.rules]
type = "array"

[inputs.rules.items.direction]
type = "string"

[inputs.rules.items.priority]
type = "number"
default = 100
"#,
        );
        let result = apply_defaults(&inputs, json!({"rules": [{"direction": "in"}]}));
        assert_eq!(
            result,
            json!({"rules": [{"direction": "in", "priority": 100}]})
        );
    }

    #[test]
    fn apply_defaults_does_not_synthesize_missing_array_parent() {
        // Symmetric with the missing-object case: defaults are substitution,
        // not synthesis. If the user omits the array entirely, we don't
        // materialize it from per-item defaults.
        let inputs = schema_for(
            r#"
[inputs.rules]
type = "array"

[inputs.rules.items.priority]
type = "number"
default = 100
"#,
        );
        let result = apply_defaults(&inputs, json!({}));
        assert_eq!(result, json!({}));
    }

    #[test]
    fn apply_defaults_does_not_touch_scalar_array_items() {
        // Arrays of scalars (e.g. dhcp_dns = [...]) have no per-item fields
        // to default. The recursion guard must skip them so user values are
        // preserved verbatim.
        let inputs = schema_for(
            r#"
[inputs.dns]
type = "array"
items = { type = "string" }
"#,
        );
        let result = apply_defaults(&inputs, json!({"dns": ["1.1.1.1", "8.8.8.8"]}));
        assert_eq!(result, json!({"dns": ["1.1.1.1", "8.8.8.8"]}));
    }

    #[test]
    fn apply_defaults_recurses_into_nested_objects_inside_array_items() {
        // array → object → object → default scalar. Confirms the array
        // branch and the existing object branch compose via mutual recursion.
        let inputs = schema_for(
            r#"
[inputs.networks]
type = "array"

[inputs.networks.items.dhcp_routes_configuration]
type = "object"

[inputs.networks.items.dhcp_routes_configuration.fields.effective_routes_auto_population]
type = "object"

[inputs.networks.items.dhcp_routes_configuration.fields.effective_routes_auto_population.fields.enabled]
type = "string"
default = "no"
"#,
        );
        let result = apply_defaults(
            &inputs,
            json!({
                "networks": [
                    {"dhcp_routes_configuration": {"effective_routes_auto_population": {}}},
                ],
            }),
        );
        assert_eq!(
            result,
            json!({
                "networks": [
                    {"dhcp_routes_configuration": {"effective_routes_auto_population": {"enabled": "no"}}},
                ],
            })
        );
    }

    #[test]
    fn apply_defaults_is_idempotent_for_array_items() {
        let inputs = schema_for(
            r#"
[inputs.rules]
type = "array"

[inputs.rules.items.priority]
type = "number"
default = 100
"#,
        );
        let once = apply_defaults(&inputs, json!({"rules": [{}]}));
        let twice = apply_defaults(&inputs, once.clone());
        assert_eq!(once, twice);
    }

    #[test]
    fn apply_defaults_with_empty_input_fills_all_defaults() {
        let inputs = schema_for(
            r#"
[inputs.tier]
type = "string"
default = "hdd"

[inputs.encrypted]
type = "string"
default = "no"
"#,
        );
        let result = apply_defaults(&inputs, json!({}));
        assert_eq!(result["tier"], "hdd");
        assert_eq!(result["encrypted"], "no");
    }

    // === validate_resolvable tests ===

    use crate::resolvable::{Resolvable, resolve_inputs};
    use std::collections::HashMap;

    #[test]
    fn validate_resolvable_passes_on_concrete_inputs() {
        let inputs = schema_for(
            r#"
[inputs.name]
type = "string"
required = true

[inputs.size]
type = "number"
"#,
        );
        let value = Resolvable::known(json!({"name": "data", "size": 100}));
        validate_resolvable(&inputs, &value).unwrap();
    }

    #[test]
    fn validate_resolvable_catches_missing_required_in_concrete() {
        let inputs = schema_for(
            r#"
[inputs.name]
type = "string"
required = true
"#,
        );
        let value = Resolvable::known(json!({}));
        let err = validate_resolvable(&inputs, &value).unwrap_err();
        assert!(err.contains("missing required field 'name'"), "got: {err}");
    }

    #[test]
    fn validate_resolvable_treats_unknown_as_present_for_required_check() {
        // service_uuid is required and pending — counts as present.
        let inputs = schema_for(
            r#"
[inputs.service_uuid]
type = "string"
required = true
"#,
        );
        let template_inputs = json!({"service_uuid": "{{ resources.svc.uuid }}"});
        let resolved = resolve_inputs(&template_inputs, &inputs, &HashMap::new()).unwrap();
        validate_resolvable(&inputs, &resolved).unwrap();
    }

    #[test]
    fn validate_resolvable_catches_missing_required_when_object_partial() {
        // Object has one Unknown field but the other required field is
        // genuinely missing.
        let inputs = schema_for(
            r#"
[inputs.name]
type = "string"
required = true

[inputs.service_uuid]
type = "string"
required = true
"#,
        );
        let template_inputs = json!({"service_uuid": "{{ resources.svc.uuid }}"});
        let resolved = resolve_inputs(&template_inputs, &inputs, &HashMap::new()).unwrap();
        let err = validate_resolvable(&inputs, &resolved).unwrap_err();
        assert!(err.contains("missing required field 'name'"), "got: {err}");
    }

    #[test]
    fn validate_resolvable_catches_unknown_field_in_partial_object() {
        let inputs = schema_for(
            r#"
[inputs.name]
type = "string"
"#,
        );
        let template_inputs = json!({
            "name": "ok",
            "typo": "{{ resources.svc.uuid }}",
        });
        let resolved = resolve_inputs(&template_inputs, &inputs, &HashMap::new()).unwrap();
        let err = validate_resolvable(&inputs, &resolved).unwrap_err();
        assert!(err.contains("unknown field 'typo'"), "got: {err}");
    }

    #[test]
    fn validate_resolvable_catches_type_mismatch_on_pending_leaf() {
        // Schema wants a number, ref's expected_type (from the upstream
        // it points at) would be a string. We don't actually consult the
        // upstream's output schema here — the resolver records the
        // *destination* expected_type. So this test is constructed to
        // ensure the destination check itself works: an Unknown that
        // claims expected_type=String but lives in a number-typed slot.
        let inputs = schema_for(
            r#"
[inputs.size]
type = "number"
"#,
        );
        // Build a Resolvable manually that simulates "schema says number,
        // pending leaf claims string" — this is what would happen if the
        // resolver were called with a permissive parent's view. In normal
        // resolver usage the Unknown would carry expected_type=Some(Number)
        // because the destination IS Number, so this asserts the check
        // catches a divergence.
        let bad_unknown = Resolvable::unknown(
            "{{ resources.svc.name }}".to_string(),
            crate::template::extract_refs("{{ resources.svc.name }}").unwrap(),
            Some(FieldType::String),
        );
        let mut map = std::collections::BTreeMap::new();
        map.insert("size".to_string(), bad_unknown);
        let value = Resolvable::Object(map);
        let err = validate_resolvable(&inputs, &value).unwrap_err();
        assert!(err.contains("expected number"), "got: {err}");
    }

    #[test]
    fn validate_resolvable_permissive_context_accepts_any_pending_type() {
        let inputs = schema_for(
            r#"
[inputs.payload]
type = "object"
"#,
        );
        let template_inputs = json!({
            "payload": {"any_key": "{{ resources.svc.uuid }}"}
        });
        let resolved = resolve_inputs(&template_inputs, &inputs, &HashMap::new()).unwrap();
        // payload is permissive (no declared fields), so the nested
        // pending leaf carries expected_type=None — accepted.
        validate_resolvable(&inputs, &resolved).unwrap();
    }

    #[test]
    fn validate_resolvable_walks_into_array_elements() {
        let inputs = schema_for(
            r#"
[inputs.tags]
type = "array"
items = { type = "string" }
"#,
        );
        let template_inputs = json!({"tags": ["a", "{{ resources.x.label }}"]});
        let resolved = resolve_inputs(&template_inputs, &inputs, &HashMap::new()).unwrap();
        validate_resolvable(&inputs, &resolved).unwrap();
    }

    #[test]
    fn validate_resolvable_recurses_into_typed_object_array() {
        let inputs = schema_for(
            r#"
[inputs.networks]
type = "array"

[inputs.networks.items.name]
type = "string"
required = true

[inputs.networks.items.uuid]
type = "string"
"#,
        );
        let template_inputs = json!({
            "networks": [
                {"name": "ok", "uuid": "{{ resources.svc.uuid }}"},
            ]
        });
        let resolved = resolve_inputs(&template_inputs, &inputs, &HashMap::new()).unwrap();
        validate_resolvable(&inputs, &resolved).unwrap();
    }

    #[test]
    fn validate_resolvable_rejects_pending_object_in_string_slot() {
        let inputs = schema_for(
            r#"
[inputs.name]
type = "string"
"#,
        );
        // Manually construct a pending Object in a String slot (would
        // happen if a user wrote a nested object literal where a string
        // is expected).
        let mut nested = std::collections::BTreeMap::new();
        nested.insert(
            "x".to_string(),
            Resolvable::unknown(
                "{{ resources.svc.uuid }}".to_string(),
                crate::template::extract_refs("{{ resources.svc.uuid }}").unwrap(),
                None,
            ),
        );
        let mut root = std::collections::BTreeMap::new();
        root.insert("name".to_string(), Resolvable::Object(nested));
        let value = Resolvable::Object(root);
        let err = validate_resolvable(&inputs, &value).unwrap_err();
        assert!(err.contains("expected string"), "got: {err}");
    }
}
