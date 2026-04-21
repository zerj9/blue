use std::collections::HashSet;

use serde_json::Value;

use crate::types::{Action, Diff, FieldDef, InputChange, Schema};

/// Diff resolved inputs against state inputs using schema metadata.
pub fn diff_resource(
    schema: &Schema,
    old_inputs: Option<&Value>,
    new_inputs: Option<&Value>,
) -> Diff {
    match (old_inputs, new_inputs) {
        (None, Some(_)) => Diff {
            action: Action::Create,
            changes: vec![],
            requires_stop: false,
        },
        (Some(_), None) => Diff {
            action: Action::Delete,
            changes: vec![],
            requires_stop: false,
        },
        (None, None) => Diff {
            action: Action::Unchanged,
            changes: vec![],
            requires_stop: false,
        },
        (Some(old), Some(new)) => diff_values(schema, old, new),
    }
}

fn diff_values(schema: &Schema, old: &Value, new: &Value) -> Diff {
    let old_obj = old.as_object();
    let new_obj = new.as_object();

    let (old_obj, new_obj) = match (old_obj, new_obj) {
        (Some(o), Some(n)) => (o, n),
        _ => {
            return Diff {
                action: Action::Unchanged,
                changes: vec![],
                requires_stop: false,
            }
        }
    };

    let all_keys: HashSet<&String> = old_obj.keys().chain(new_obj.keys()).collect();
    let mut changes = Vec::new();
    let mut has_force_new = false;
    let mut has_requires_stop = false;

    for key in all_keys {
        let field_def = schema.inputs.iter().find(|f| f.path == *key);
        let force_new = field_def.map_or(false, |f| f.force_new);
        let requires_stop = field_def.map_or(false, |f| f.requires_stop);

        let child_fields = field_def.map(|f| f.items.as_slice()).unwrap_or(&[]);

        let changed = match (old_obj.get(key), new_obj.get(key)) {
            (None, Some(new_val)) => {
                changes.push(InputChange::Added {
                    field: key.clone(),
                    value: new_val.clone(),
                    force_new,
                    requires_stop,
                });
                true
            }
            (Some(old_val), None) => {
                changes.push(InputChange::Removed {
                    field: key.clone(),
                    value: old_val.clone(),
                    force_new,
                    requires_stop,
                });
                true
            }
            (Some(old_val), Some(new_val)) if !values_equal(old_val, new_val, field_def, child_fields) => {
                changes.push(InputChange::Modified {
                    field: key.clone(),
                    old_value: old_val.clone(),
                    new_value: new_val.clone(),
                    force_new,
                    requires_stop,
                });
                true
            }
            _ => false,
        };

        if changed {
            has_force_new |= force_new;
            has_requires_stop |= requires_stop;
        }
    }

    let action = if changes.is_empty() {
        Action::Unchanged
    } else if has_force_new {
        Action::Replace
    } else {
        Action::Update
    };

    Diff {
        action,
        changes,
        requires_stop: has_requires_stop,
    }
}

/// Compare two values for equality, using schema metadata for nested structure.
///
/// `field_def` is the schema definition for the current value (provides `ordered` for arrays).
/// `fields` is the list of child FieldDefs for looking up keys when the value is an object.
fn values_equal(old: &Value, new: &Value, field_def: Option<&FieldDef>, fields: &[FieldDef]) -> bool {
    match (old, new) {
        (Value::Number(a), Value::Number(b)) => {
            // Numeric comparison: 10 == 10.0
            a.as_f64() == b.as_f64()
        }
        (Value::Array(a), Value::Array(b)) => {
            let ordered = field_def.map_or(true, |f| f.ordered);
            let items = field_def.map(|f| f.items.as_slice()).unwrap_or(&[]);
            let (item_def, item_fields) = item_schema(items);
            if ordered {
                // Positional comparison
                a.len() == b.len()
                    && a.iter().zip(b.iter()).all(|(av, bv)| values_equal(av, bv, item_def, item_fields))
            } else {
                // Set comparison via canonical serialization
                if a.len() != b.len() {
                    return false;
                }
                let old_hashes: HashSet<String> = a.iter().map(|v| canonical_string(v, item_def, item_fields)).collect();
                let new_hashes: HashSet<String> = b.iter().map(|v| canonical_string(v, item_def, item_fields)).collect();
                old_hashes == new_hashes
            }
        }
        (Value::Object(a), Value::Object(b)) => {
            let keys: HashSet<&String> = a.keys().chain(b.keys()).collect();
            keys.iter().all(|k| match (a.get(*k), b.get(*k)) {
                (Some(av), Some(bv)) => {
                    let child_def = fields.iter().find(|f| f.path == **k);
                    let child_fields = child_def.map(|f| f.items.as_slice()).unwrap_or(&[]);
                    values_equal(av, bv, child_def, child_fields)
                }
                _ => false,
            })
        }
        _ => old == new,
    }
}

/// For array items, determine the element-level FieldDef and child fields.
///
/// - Single unnamed item (`items = { type = "string" }`): returns the item's FieldDef
/// - Named items (object fields): returns None for the element def, items as child fields
fn item_schema(items: &[FieldDef]) -> (Option<&FieldDef>, &[FieldDef]) {
    if items.len() == 1 && items[0].path.is_empty() {
        (Some(&items[0]), &items[0].items)
    } else {
        (None, items)
    }
}

/// Produce a canonical string for set comparison of unordered arrays.
/// Objects have keys sorted so {b:1, a:2} == {a:2, b:1}.
/// Nested unordered arrays have elements sorted so [2,1] == [1,2].
fn canonical_string(value: &Value, field_def: Option<&FieldDef>, fields: &[FieldDef]) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let entries: Vec<String> = keys
                .iter()
                .map(|k| {
                    let child_def = fields.iter().find(|f| f.path == **k);
                    let child_fields = child_def.map(|f| f.items.as_slice()).unwrap_or(&[]);
                    format!("{}:{}", k, canonical_string(&map[*k], child_def, child_fields))
                })
                .collect();
            format!("{{{}}}", entries.join(","))
        }
        Value::Array(arr) => {
            let ordered = field_def.map_or(true, |f| f.ordered);
            let items = field_def.map(|f| f.items.as_slice()).unwrap_or(&[]);
            let (item_def, item_fields) = item_schema(items);
            let mut strs: Vec<String> = arr.iter()
                .map(|v| canonical_string(v, item_def, item_fields))
                .collect();
            if !ordered {
                strs.sort();
            }
            format!("[{}]", strs.join(","))
        }
        _ => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::parse_schema;
    use serde_json::json;

    fn test_schema() -> Schema {
        parse_schema(
            r#"
[inputs.hostname]
type = "string"
required = true
force_new = true

[inputs.plan]
type = "string"
requires_stop = true

[inputs.tags]
type = "array"
ordered = false
items = { type = "string" }

[inputs.firewall_rules]
type = "array"

[outputs.uuid]
type = "string"
"#,
        )
        .unwrap()
    }

    #[test]
    fn create_when_no_old() {
        let schema = test_schema();
        let diff = diff_resource(&schema, None, Some(&json!({"hostname": "web-01"})));
        assert_eq!(diff.action, Action::Create);
    }

    #[test]
    fn delete_when_no_new() {
        let schema = test_schema();
        let diff = diff_resource(&schema, Some(&json!({"hostname": "web-01"})), None);
        assert_eq!(diff.action, Action::Delete);
    }

    #[test]
    fn unchanged() {
        let schema = test_schema();
        let val = json!({"hostname": "web-01", "plan": "1xCPU"});
        let diff = diff_resource(&schema, Some(&val), Some(&val));
        assert_eq!(diff.action, Action::Unchanged);
        assert!(diff.changes.is_empty());
    }

    #[test]
    fn update_non_force_new() {
        let schema = test_schema();
        let old = json!({"hostname": "web-01", "plan": "1xCPU"});
        let new = json!({"hostname": "web-01", "plan": "2xCPU"});
        let diff = diff_resource(&schema, Some(&old), Some(&new));
        assert_eq!(diff.action, Action::Update);
        assert!(diff.requires_stop);
        assert_eq!(diff.changes.len(), 1);
    }

    #[test]
    fn replace_on_force_new() {
        let schema = test_schema();
        let old = json!({"hostname": "web-01"});
        let new = json!({"hostname": "web-02"});
        let diff = diff_resource(&schema, Some(&old), Some(&new));
        assert_eq!(diff.action, Action::Replace);
    }

    #[test]
    fn numeric_equality() {
        let schema = test_schema();
        let old = json!({"hostname": "web-01", "plan": "1xCPU"});
        let new = json!({"hostname": "web-01", "plan": "1xCPU"});
        let diff = diff_resource(&schema, Some(&old), Some(&new));
        assert_eq!(diff.action, Action::Unchanged);
    }

    #[test]
    fn number_int_float_equal() {
        let schema = parse_schema(
            r#"
[inputs.size]
type = "number"

[outputs.id]
type = "string"
"#,
        )
        .unwrap();
        let old = json!({"size": 10});
        let new = json!({"size": 10.0});
        let diff = diff_resource(&schema, Some(&old), Some(&new));
        assert_eq!(diff.action, Action::Unchanged);
    }

    #[test]
    fn unordered_array_ignores_order() {
        let schema = test_schema();
        let old = json!({"tags": ["a", "b", "c"]});
        let new = json!({"tags": ["c", "a", "b"]});
        let diff = diff_resource(&schema, Some(&old), Some(&new));
        assert_eq!(diff.action, Action::Unchanged);
    }

    #[test]
    fn unordered_array_detects_change() {
        let schema = test_schema();
        let old = json!({"tags": ["a", "b"]});
        let new = json!({"tags": ["a", "c"]});
        let diff = diff_resource(&schema, Some(&old), Some(&new));
        assert_eq!(diff.action, Action::Update);
    }

    #[test]
    fn ordered_array_detects_reorder() {
        let schema = test_schema();
        let old = json!({"firewall_rules": [{"port": 80}, {"port": 443}]});
        let new = json!({"firewall_rules": [{"port": 443}, {"port": 80}]});
        let diff = diff_resource(&schema, Some(&old), Some(&new));
        assert_eq!(diff.action, Action::Update);
    }

    #[test]
    fn added_field() {
        let schema = test_schema();
        let old = json!({"hostname": "web-01"});
        let new = json!({"hostname": "web-01", "plan": "1xCPU"});
        let diff = diff_resource(&schema, Some(&old), Some(&new));
        assert_eq!(diff.action, Action::Update);
        assert!(matches!(&diff.changes[0], InputChange::Added { field, .. } if field == "plan"));
    }

    #[test]
    fn removed_field() {
        let schema = test_schema();
        let old = json!({"hostname": "web-01", "plan": "1xCPU"});
        let new = json!({"hostname": "web-01"});
        let diff = diff_resource(&schema, Some(&old), Some(&new));
        assert_eq!(diff.action, Action::Update);
        assert!(matches!(&diff.changes[0], InputChange::Removed { field, .. } if field == "plan"));
    }

    #[test]
    fn nested_unordered_array_ignores_order() {
        let schema = parse_schema(r#"
[inputs.rules]
type = "array"

[inputs.rules.items.name]
type = "string"

[inputs.rules.items.ports]
type = "array"
ordered = false
items = { type = "number" }

[outputs.id]
type = "string"
"#).unwrap();

        let old = json!({"rules": [{"name": "web", "ports": [80, 443]}]});
        let new = json!({"rules": [{"name": "web", "ports": [443, 80]}]});
        let diff = diff_resource(&schema, Some(&old), Some(&new));
        assert_eq!(diff.action, Action::Unchanged);
    }

    #[test]
    fn nested_unordered_array_detects_change() {
        let schema = parse_schema(r#"
[inputs.rules]
type = "array"

[inputs.rules.items.name]
type = "string"

[inputs.rules.items.ports]
type = "array"
ordered = false
items = { type = "number" }

[outputs.id]
type = "string"
"#).unwrap();

        let old = json!({"rules": [{"name": "web", "ports": [80, 443]}]});
        let new = json!({"rules": [{"name": "web", "ports": [80, 8080]}]});
        let diff = diff_resource(&schema, Some(&old), Some(&new));
        assert_eq!(diff.action, Action::Update);
    }
}
