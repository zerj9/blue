use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::provider::{OperationCtx, ResourceType};
use crate::types::{OperationResult, Schema};

use super::client::UpCloudClient;

const SCHEMA: &str = include_str!("schemas/upcloud_storage_resource.toml");

/// UpCloud storage fields exposed as Blue outputs (downstream resources can
/// reference these via `{{ resources.X.<field> }}`). `delete_backups` is
/// mirrored separately from inputs.
const UPCLOUD_OUTPUT_FIELDS: &[&str] =
    &["uuid", "state", "access", "type", "zone", "size", "title"];

pub struct UpCloudStorageResource {
    schema: Schema,
    client: Arc<UpCloudClient>,
}

#[derive(Deserialize)]
struct StorageDetailResponse {
    storage: Value,
}

impl UpCloudStorageResource {
    pub fn new(client: Arc<UpCloudClient>) -> Self {
        let schema = crate::schema::parse_schema(SCHEMA)
            .expect("upcloud storage resource schema must be valid");
        UpCloudStorageResource { schema, client }
    }

    fn create_storage(&self, inputs: &Value) -> Result<Value, String> {
        let body = build_create_body(inputs);
        let path = "/storage";
        let mut resp = self.client.post(path, &body)?;
        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let err_body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(format!(
                "upcloud POST {path} failed: http status: {status}: {err_body}"
            ));
        }
        let wrapper: StorageDetailResponse = resp
            .body_mut()
            .read_json()
            .map_err(|e| format!("upcloud POST {path} response parse failed: {e}"))?;
        Ok(wrapper.storage)
    }

    fn modify_storage(&self, uuid: &str, inputs: &Value) -> Result<Value, String> {
        let body = build_modify_body(inputs);
        let path = format!("/storage/{uuid}");
        let mut resp = self.client.put(&path, &body)?;
        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let err_body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(format!(
                "upcloud PUT {path} failed: http status: {status}: {err_body}"
            ));
        }
        let wrapper: StorageDetailResponse = resp
            .body_mut()
            .read_json()
            .map_err(|e| format!("upcloud PUT {path} response parse failed: {e}"))?;
        Ok(wrapper.storage)
    }

    /// Fetch a single storage by UUID. Returns Ok(None) if the storage no
    /// longer exists at the provider (HTTP 404), letting the caller signal
    /// drift via OperationResult::NotFound so Blue removes it from state.
    fn get_storage(&self, uuid: &str) -> Result<Option<Value>, String> {
        let path = format!("/storage/{uuid}");
        let mut resp = self.client.get(&path)?;
        let status = resp.status().as_u16();
        if status == 404 {
            return Ok(None);
        }
        if !resp.status().is_success() {
            let err_body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(format!(
                "upcloud GET {path} failed: http status: {status}: {err_body}"
            ));
        }
        let wrapper: StorageDetailResponse = resp
            .body_mut()
            .read_json()
            .map_err(|e| format!("upcloud GET {path} response parse failed: {e}"))?;
        Ok(Some(wrapper.storage))
    }
}

impl ResourceType for UpCloudStorageResource {
    fn schema(&self) -> &Schema {
        &self.schema
    }

    fn create(
        &self,
        ctx: &dyn OperationCtx,
        inputs: Value,
    ) -> Result<OperationResult, String> {
        let storage = self.create_storage(&inputs)?;

        // Persist the uuid as soon as we have it, so a crash before this
        // function returns doesn't strand the resource.
        if let Some(uuid) = storage.get("uuid").and_then(|v| v.as_str()) {
            ctx.save(&json!({ "uuid": uuid }));
        }

        let outputs = extract_outputs(&storage, inputs.get("delete_backups"));
        Ok(OperationResult::Success { outputs })
    }

    fn read(&self, outputs: &Value) -> Result<OperationResult, String> {
        let uuid = outputs
            .get("uuid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "upcloud.storage read: missing 'uuid' in stored outputs".to_string())?;

        match self.get_storage(uuid)? {
            Some(storage) => {
                // delete_backups isn't a UpCloud field — carry it forward
                // from the previously stored outputs so it survives refresh.
                let new_outputs = extract_outputs(&storage, outputs.get("delete_backups"));
                Ok(OperationResult::Success { outputs: new_outputs })
            }
            None => Ok(OperationResult::NotFound),
        }
    }

    fn update(
        &self,
        _ctx: &dyn OperationCtx,
        old_outputs: &Value,
        new_inputs: Value,
    ) -> Result<OperationResult, String> {
        let uuid = old_outputs
            .get("uuid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.storage update: missing 'uuid' in old outputs".to_string()
            })?;
        let storage = self.modify_storage(uuid, &new_inputs)?;
        let outputs = extract_outputs(&storage, new_inputs.get("delete_backups"));
        Ok(OperationResult::Success { outputs })
    }

    fn delete(
        &self,
        _ctx: &dyn OperationCtx,
        outputs: &Value,
    ) -> Result<OperationResult, String> {
        let uuid = outputs
            .get("uuid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.storage delete: missing 'uuid' in stored outputs".to_string()
            })?;
        let backups = outputs
            .get("delete_backups")
            .and_then(|v| v.as_str())
            .unwrap_or("keep");
        let path = format!("/storage/{uuid}?backups={backups}");
        let mut resp = self.client.delete(&path)?;
        let status = resp.status().as_u16();

        // 404 = already gone; treat as idempotent success.
        if status == 404 {
            return Ok(OperationResult::Success { outputs: json!({}) });
        }

        if !resp.status().is_success() {
            let err_body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(format!(
                "upcloud DELETE {path} failed: http status: {status}: {err_body}"
            ));
        }

        Ok(OperationResult::Success { outputs: json!({}) })
    }

    fn validate(&self, inputs: &Value) -> Result<(), String> {
        // Catch bad `delete_backups` values at plan time. This is the only
        // input we actually consume directly (it lands in a URL query
        // parameter); other field values are validated by UpCloud at API time.
        if let Some(v) = inputs.get("delete_backups").and_then(|v| v.as_str()) {
            match v {
                "keep" | "keep_latest" | "delete" => {}
                other => {
                    return Err(format!(
                        "delete_backups must be 'keep', 'keep_latest', or 'delete'; got '{other}'"
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Build the JSON body for `POST /storage`. Required fields (`title`, `size`,
/// `zone`) are guaranteed present by schema validation; optional fields are
/// only included if the user supplied them so UpCloud applies its own defaults.
///
/// UpCloud accepts numeric fields (`size`, `retention`) as strings — converted
/// here. Response-side parsing reads them back as numbers.
fn build_create_body(inputs: &Value) -> Value {
    let mut storage = Map::new();

    if let Some(v) = inputs.get("title") {
        storage.insert("title".to_string(), v.clone());
    }
    if let Some(v) = inputs.get("size") {
        storage.insert("size".to_string(), to_string_value(v));
    }
    if let Some(v) = inputs.get("zone") {
        storage.insert("zone".to_string(), v.clone());
    }
    if let Some(v) = inputs.get("tier") {
        storage.insert("tier".to_string(), v.clone());
    }
    if let Some(v) = inputs.get("encrypted") {
        storage.insert("encrypted".to_string(), v.clone());
    }

    if let Some(br) = inputs.get("backup_rule").and_then(|v| v.as_object()) {
        let mut rule = Map::new();
        if let Some(v) = br.get("interval") {
            rule.insert("interval".to_string(), v.clone());
        }
        if let Some(v) = br.get("time") {
            rule.insert("time".to_string(), v.clone());
        }
        if let Some(v) = br.get("retention") {
            rule.insert("retention".to_string(), to_string_value(v));
        }
        if !rule.is_empty() {
            storage.insert("backup_rule".to_string(), Value::Object(rule));
        }
    }

    if let Some(labels) = inputs.get("labels").and_then(|v| v.as_array()) {
        if !labels.is_empty() {
            storage.insert("labels".to_string(), Value::Array(labels.clone()));
        }
    }

    json!({ "storage": storage })
}

/// Build the JSON body for `PUT /storage/{uuid}`. Only the fields UpCloud
/// allows modifying in place — `title`, `size`, `backup_rule`, `labels` —
/// are included. Force_new fields (`zone`, `tier`, `encrypted`) are never
/// modified in place; changes to them produce a Replace action that goes
/// through `delete` + `create` instead of this code path.
///
/// Same number-as-string quirk as `build_create_body` for `size` and
/// `backup_rule.retention`.
///
/// Limitation: removing `backup_rule` or `labels` from config doesn't clear
/// them from UpCloud — if absent in inputs, the field is omitted from the
/// PUT body and UpCloud leaves the previous value. To clear, deploy with an
/// explicit empty value (not yet supported in phase 1).
fn build_modify_body(inputs: &Value) -> Value {
    let mut storage = Map::new();

    if let Some(v) = inputs.get("title") {
        storage.insert("title".to_string(), v.clone());
    }
    if let Some(v) = inputs.get("size") {
        storage.insert("size".to_string(), to_string_value(v));
    }

    if let Some(br) = inputs.get("backup_rule").and_then(|v| v.as_object()) {
        let mut rule = Map::new();
        if let Some(v) = br.get("interval") {
            rule.insert("interval".to_string(), v.clone());
        }
        if let Some(v) = br.get("time") {
            rule.insert("time".to_string(), v.clone());
        }
        if let Some(v) = br.get("retention") {
            rule.insert("retention".to_string(), to_string_value(v));
        }
        if !rule.is_empty() {
            storage.insert("backup_rule".to_string(), Value::Object(rule));
        }
    }

    if let Some(labels) = inputs.get("labels").and_then(|v| v.as_array()) {
        if !labels.is_empty() {
            storage.insert("labels".to_string(), Value::Array(labels.clone()));
        }
    }

    json!({ "storage": storage })
}

/// Extract Blue outputs from a UpCloud storage response. Only the fields
/// declared in `UPCLOUD_OUTPUT_FIELDS` are exposed plus the mirrored
/// `delete_backups` value carried from inputs (or its default).
fn extract_outputs(storage: &Value, delete_backups: Option<&Value>) -> Value {
    let mut out = Map::new();
    if let Some(obj) = storage.as_object() {
        for &field in UPCLOUD_OUTPUT_FIELDS {
            if let Some(v) = obj.get(field) {
                out.insert(field.to_string(), v.clone());
            }
        }
    }
    out.insert(
        "delete_backups".to_string(),
        delete_backups
            .cloned()
            .unwrap_or_else(|| Value::String("keep".to_string())),
    );
    Value::Object(out)
}

/// Coerce numeric `Value` to its decimal string representation. UpCloud's
/// API accepts `size` and `retention` as strings, even though responses
/// return them as numbers.
fn to_string_value(v: &Value) -> Value {
    if let Some(n) = v.as_i64() {
        Value::String(n.to_string())
    } else if let Some(n) = v.as_u64() {
        Value::String(n.to_string())
    } else if let Some(n) = v.as_f64() {
        Value::String((n as i64).to_string())
    } else {
        v.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn build_create_body_minimal() {
        let inputs = json!({
            "title": "data-disk",
            "size": 100,
            "zone": "uk-lon1",
        });
        let body = build_create_body(&inputs);
        assert_eq!(
            body,
            json!({
                "storage": {
                    "title": "data-disk",
                    "size": "100",  // stringified per UpCloud's quirk
                    "zone": "uk-lon1",
                }
            })
        );
    }

    #[test]
    fn build_create_body_full() {
        let inputs = json!({
            "title": "production-db",
            "size": 200,
            "zone": "fi-hel1",
            "tier": "maxiops",
            "encrypted": "yes",
            "backup_rule": {
                "interval": "daily",
                "time": "0430",
                "retention": 14,
            },
            "labels": [
                {"key": "env", "value": "prod"},
            ],
            "delete_backups": "keep_latest",
        });
        let body = build_create_body(&inputs);
        let storage = body.get("storage").unwrap();
        assert_eq!(storage.get("title").unwrap(), "production-db");
        assert_eq!(storage.get("size").unwrap(), "200");
        assert_eq!(storage.get("zone").unwrap(), "fi-hel1");
        assert_eq!(storage.get("tier").unwrap(), "maxiops");
        assert_eq!(storage.get("encrypted").unwrap(), "yes");
        assert_eq!(storage.get("backup_rule").unwrap().get("retention").unwrap(), "14");
        assert_eq!(storage.get("backup_rule").unwrap().get("time").unwrap(), "0430");
        assert!(storage.get("labels").unwrap().as_array().unwrap().len() == 1);
        // delete_backups is a Blue-side concern; should NOT be sent to UpCloud
        assert!(storage.get("delete_backups").is_none());
    }

    #[test]
    fn build_create_body_skips_optional_fields_when_absent() {
        let inputs = json!({
            "title": "minimal",
            "size": 10,
            "zone": "uk-lon1",
        });
        let body = build_create_body(&inputs);
        let storage = body.get("storage").unwrap().as_object().unwrap();
        assert!(!storage.contains_key("tier"));
        assert!(!storage.contains_key("encrypted"));
        assert!(!storage.contains_key("backup_rule"));
        assert!(!storage.contains_key("labels"));
    }

    #[test]
    fn extract_outputs_picks_declared_fields_plus_delete_backups() {
        let storage = json!({
            "uuid": "abc-123",
            "state": "online",
            "access": "private",
            "type": "normal",
            "zone": "uk-lon1",
            "size": 100,
            "title": "data-disk",
            "tier": "maxiops",         // not exposed
            "license": 0,              // not exposed
            "encrypted": "no",         // not exposed
        });
        let outputs = extract_outputs(&storage, Some(&json!("delete")));
        let obj = outputs.as_object().unwrap();
        assert_eq!(obj.len(), 8); // 7 upcloud + delete_backups
        assert_eq!(obj["uuid"], "abc-123");
        assert_eq!(obj["state"], "online");
        assert_eq!(obj["delete_backups"], "delete");
        assert!(!obj.contains_key("tier"));
        assert!(!obj.contains_key("license"));
        assert!(!obj.contains_key("encrypted"));
    }

    #[test]
    fn extract_outputs_defaults_delete_backups_to_keep() {
        let storage = json!({"uuid": "abc"});
        let outputs = extract_outputs(&storage, None);
        assert_eq!(outputs.get("delete_backups").unwrap(), "keep");
    }

    #[test]
    fn build_modify_body_includes_only_modifiable_fields() {
        let inputs = json!({
            "title": "renamed",
            "size": 200,
            "zone": "uk-lon1",       // force_new — must NOT appear in PUT body
            "tier": "maxiops",       // force_new — must NOT appear
            "encrypted": "yes",      // force_new — must NOT appear
            "delete_backups": "keep", // Blue-side only — must NOT appear
        });
        let body = build_modify_body(&inputs);
        let storage = body.get("storage").unwrap().as_object().unwrap();
        assert_eq!(storage.get("title").unwrap(), "renamed");
        assert_eq!(storage.get("size").unwrap(), "200");
        assert!(!storage.contains_key("zone"));
        assert!(!storage.contains_key("tier"));
        assert!(!storage.contains_key("encrypted"));
        assert!(!storage.contains_key("delete_backups"));
    }

    #[test]
    fn build_modify_body_serializes_backup_rule_with_stringified_retention() {
        let inputs = json!({
            "title": "x",
            "size": 100,
            "backup_rule": {
                "interval": "mon",
                "time": "0200",
                "retention": 7,
            },
        });
        let body = build_modify_body(&inputs);
        let rule = body
            .get("storage")
            .unwrap()
            .get("backup_rule")
            .unwrap();
        assert_eq!(rule.get("interval").unwrap(), "mon");
        assert_eq!(rule.get("time").unwrap(), "0200");
        assert_eq!(rule.get("retention").unwrap(), "7");
    }

    #[test]
    fn build_modify_body_omits_absent_optional_fields() {
        let inputs = json!({
            "title": "renamed",
            "size": 100,
        });
        let body = build_modify_body(&inputs);
        let storage = body.get("storage").unwrap().as_object().unwrap();
        assert_eq!(storage.len(), 2); // title + size only
        assert!(!storage.contains_key("backup_rule"));
        assert!(!storage.contains_key("labels"));
    }
}
