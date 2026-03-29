mod upcloud;

use std::collections::HashMap;

use crate::provider::{ProviderMode, ProviderRegistry};

pub fn build_registry(mode: ProviderMode) -> ProviderRegistry {
    let mut registry = ProviderRegistry::new(mode);
    registry.register("upcloud", |mode| Ok(Box::new(upcloud::Client::new(mode)?)));
    registry
}

/// Convert a JSON value to a string for filter matching.
pub fn json_as_str(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Check if a JSON object matches a set of filters.
/// Fields listed in `substring_fields` use substring matching; all others use exact match.
pub fn matches_filters(
    item: &serde_json::Value,
    filters: &HashMap<String, String>,
    substring_fields: &[&str],
) -> bool {
    let obj = match item.as_object() {
        Some(o) => o,
        None => return false,
    };
    for (key, value) in filters {
        let field_value = match obj.get(key.as_str()).and_then(json_as_str) {
            Some(fv) => fv,
            None => return false,
        };
        if substring_fields.contains(&key.as_str()) {
            if !field_value.contains(value.as_str()) {
                return false;
            }
        } else if field_value != *value {
            return false;
        }
    }
    true
}
