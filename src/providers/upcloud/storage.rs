use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::provider::DataSourceType;
use crate::types::Schema;

use super::client::UpCloudClient;

const SCHEMA: &str = include_str!("schemas/upcloud_storage_data_source.toml");
const OUTPUT_FIELDS: &[&str] = &["uuid", "title", "size", "zone", "type"];

pub struct UpCloudStorageDataSource {
    schema: Schema,
    client: Arc<UpCloudClient>,
}

#[derive(Deserialize)]
struct StorageListResponse {
    storages: StorageListInner,
}

#[derive(Deserialize)]
struct StorageListInner {
    storage: Vec<Value>,
}

impl UpCloudStorageDataSource {
    pub fn new(client: Arc<UpCloudClient>) -> Self {
        let schema = crate::schema::parse_schema(SCHEMA)
            .expect("upcloud storage schema must be valid");
        UpCloudStorageDataSource { schema, client }
    }

    fn list_storage(&self) -> Result<Vec<Value>, String> {
        let resp: StorageListResponse = self.client.get("/storage")?;
        Ok(resp.storages.storage)
    }
}

impl DataSourceType for UpCloudStorageDataSource {
    fn schema(&self) -> &Schema {
        &self.schema
    }

    fn read(&self, inputs: Value) -> Result<Value, String> {
        let filters = inputs
            .get("filters")
            .and_then(|v| v.as_object())
            .ok_or_else(|| "missing required input 'filters'".to_string())?;

        let storages = self.list_storage()?;
        let matched = find_match(&storages, filters)?;
        Ok(extract_outputs(matched))
    }
}

fn find_match<'a>(
    storages: &'a [Value],
    filters: &Map<String, Value>,
) -> Result<&'a Value, String> {
    let matches: Vec<&Value> = storages
        .iter()
        .filter(|s| storage_matches(s, filters))
        .collect();

    match matches.len() {
        0 => Err(format!(
            "no upcloud storage matched filters {}",
            filters_to_string(filters)
        )),
        1 => Ok(matches[0]),
        n => Err(format!(
            "{n} upcloud storage entries matched filters {}; expected exactly one",
            filters_to_string(filters)
        )),
    }
}

fn storage_matches(storage: &Value, filters: &Map<String, Value>) -> bool {
    let obj = match storage.as_object() {
        Some(o) => o,
        None => return false,
    };
    filters.iter().all(|(k, v)| obj.get(k) == Some(v))
}

fn extract_outputs(storage: &Value) -> Value {
    let mut out = Map::new();
    if let Some(obj) = storage.as_object() {
        for &field in OUTPUT_FIELDS {
            if let Some(v) = obj.get(field) {
                out.insert(field.to_string(), v.clone());
            }
        }
    }
    Value::Object(out)
}

fn filters_to_string(filters: &Map<String, Value>) -> String {
    let pairs: Vec<String> = filters.iter().map(|(k, v)| format!("{k}={v}")).collect();
    format!("{{{}}}", pairs.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn filter_map(pairs: &[(&str, Value)]) -> Map<String, Value> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    #[test]
    fn schema_parses() {
        let _ = crate::schema::parse_schema(SCHEMA).unwrap();
    }

    #[test]
    fn find_match_happy_path() {
        let storages = vec![
            json!({"uuid": "a", "title": "Alpha", "type": "template"}),
            json!({"uuid": "b", "title": "Beta", "type": "template"}),
            json!({"uuid": "c", "title": "Gamma", "type": "normal"}),
        ];
        let filters = filter_map(&[("title", json!("Beta"))]);
        let matched = find_match(&storages, &filters).unwrap();
        assert_eq!(matched["uuid"], "b");
    }

    #[test]
    fn find_match_zero_matches_errors_with_filter_summary() {
        let storages = vec![json!({"uuid": "a", "title": "Alpha"})];
        let filters = filter_map(&[("title", json!("Nonexistent"))]);
        let err = find_match(&storages, &filters).unwrap_err();
        assert!(err.contains("no upcloud storage matched"), "got: {err}");
        assert!(err.contains("title="), "got: {err}");
    }

    #[test]
    fn find_match_multiple_matches_errors_with_count() {
        let storages = vec![
            json!({"uuid": "a", "type": "template"}),
            json!({"uuid": "b", "type": "template"}),
        ];
        let filters = filter_map(&[("type", json!("template"))]);
        let err = find_match(&storages, &filters).unwrap_err();
        assert!(err.contains("2 upcloud storage entries matched"), "got: {err}");
        assert!(err.contains("expected exactly one"), "got: {err}");
    }

    #[test]
    fn find_match_uses_and_logic_across_filter_keys() {
        let storages = vec![
            json!({"uuid": "a", "type": "template", "title": "Ubuntu"}),
            json!({"uuid": "b", "type": "template", "title": "Debian"}),
            json!({"uuid": "c", "type": "normal", "title": "Ubuntu"}),
        ];
        let filters = filter_map(&[("type", json!("template")), ("title", json!("Ubuntu"))]);
        let matched = find_match(&storages, &filters).unwrap();
        assert_eq!(matched["uuid"], "a");
    }

    #[test]
    fn find_match_filter_on_unknown_key_yields_no_match() {
        let storages = vec![json!({"uuid": "a", "title": "Alpha"})];
        let filters = filter_map(&[("nonexistent_key", json!("x"))]);
        let err = find_match(&storages, &filters).unwrap_err();
        assert!(err.contains("no upcloud storage matched"), "got: {err}");
    }

    #[test]
    fn extract_outputs_picks_only_declared_fields() {
        let storage = json!({
            "uuid": "abc",
            "title": "Ubuntu",
            "size": 5,
            "type": "template",
            "access": "public",
            "encrypted": "no",
            "license": 0,
        });
        let outputs = extract_outputs(&storage);
        let obj = outputs.as_object().unwrap();
        assert!(obj.contains_key("uuid"));
        assert!(obj.contains_key("title"));
        assert!(obj.contains_key("size"));
        assert!(obj.contains_key("type"));
        assert!(!obj.contains_key("access"));
        assert!(!obj.contains_key("encrypted"));
        assert!(!obj.contains_key("license"));
    }

    #[test]
    fn extract_outputs_omits_missing_fields() {
        // Public templates often have no `zone`. Output should just omit it
        // rather than insert null.
        let storage = json!({
            "uuid": "abc",
            "title": "Ubuntu",
            "size": 5,
            "type": "template",
        });
        let outputs = extract_outputs(&storage);
        let obj = outputs.as_object().unwrap();
        assert!(!obj.contains_key("zone"));
        assert_eq!(obj["uuid"], "abc");
    }

    #[test]
    #[ignore = "requires UPCLOUD_TOKEN or UPCLOUD_USERNAME+UPCLOUD_PASSWORD; run with --ignored"]
    fn list_storage_integration() {
        use base64::Engine;
        use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

        let header = if let Ok(token) = std::env::var("UPCLOUD_TOKEN") {
            format!("Bearer {token}")
        } else {
            let username = std::env::var("UPCLOUD_USERNAME")
                .expect("UPCLOUD_TOKEN or UPCLOUD_USERNAME must be set");
            let password = std::env::var("UPCLOUD_PASSWORD")
                .expect("UPCLOUD_PASSWORD must be set when using UPCLOUD_USERNAME");
            format!("Basic {}", BASE64_STANDARD.encode(format!("{username}:{password}")))
        };

        let client = Arc::new(UpCloudClient::new(header));
        let ds = UpCloudStorageDataSource::new(client);
        let storages = ds.list_storage().expect("list_storage should succeed");
        assert!(
            !storages.is_empty(),
            "account should have at least public templates accessible"
        );
    }
}
