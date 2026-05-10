use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::provider::{OperationCtx, ResourceType};
use crate::resolvable::Resolvable;
use crate::types::{OperationResult, Schema};

use super::client::UpCloudClient;

const SCHEMA: &str =
    include_str!("schemas/upcloud_managed_object_storage_user_access_key_resource.toml");

/// Output fields surfaced from UpCloud's responses. `service_uuid` and
/// `username` are added separately (mirrored from inputs — the API
/// response includes neither). `last_used_at` is deliberately not
/// surfaced: it mutates whenever the key is used by any S3 client out of
/// band, which would produce drift on every refresh.
const OUTPUT_FIELDS: &[&str] = &[
    "access_key_id",
    "secret_access_key",
    "status",
    "created_at",
];

pub struct UpCloudManagedObjectStorageUserAccessKeyResource {
    schema: Schema,
    client: Arc<UpCloudClient>,
}

impl UpCloudManagedObjectStorageUserAccessKeyResource {
    pub fn new(client: Arc<UpCloudClient>) -> Self {
        let schema = crate::schema::parse_schema(SCHEMA)
            .expect("upcloud managed_object_storage_user_access_key schema must be valid");
        UpCloudManagedObjectStorageUserAccessKeyResource { schema, client }
    }

    fn create_key(&self, service_uuid: &str, username: &str) -> Result<Value, String> {
        let path = format!("/object-storage-2/{service_uuid}/users/{username}/access-keys");
        // The API takes no fields on create (UpCloud generates the key
        // pair). Send an empty JSON body explicitly.
        let body = json!({});
        let mut resp = self.client.post(&path, &body)?;
        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let err_body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(format!(
                "upcloud POST {path} failed: http status: {status}: {err_body}"
            ));
        }
        let key: Value = resp
            .body_mut()
            .read_json()
            .map_err(|e| format!("upcloud POST {path} response parse failed: {e}"))?;
        Ok(key)
    }

    fn get_key(
        &self,
        service_uuid: &str,
        username: &str,
        access_key_id: &str,
    ) -> Result<Option<Value>, String> {
        let path = format!(
            "/object-storage-2/{service_uuid}/users/{username}/access-keys/{access_key_id}"
        );
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
        let key: Value = resp
            .body_mut()
            .read_json()
            .map_err(|e| format!("upcloud GET {path} response parse failed: {e}"))?;
        Ok(Some(key))
    }

    fn patch_status(
        &self,
        service_uuid: &str,
        username: &str,
        access_key_id: &str,
        status: &str,
    ) -> Result<Value, String> {
        let path = format!(
            "/object-storage-2/{service_uuid}/users/{username}/access-keys/{access_key_id}"
        );
        let body = json!({ "status": status });
        let mut resp = self.client.patch(&path, &body)?;
        let http_status = resp.status().as_u16();
        if !resp.status().is_success() {
            let err_body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(format!(
                "upcloud PATCH {path} failed: http status: {http_status}: {err_body}"
            ));
        }
        let key: Value = resp
            .body_mut()
            .read_json()
            .map_err(|e| format!("upcloud PATCH {path} response parse failed: {e}"))?;
        Ok(key)
    }
}

impl ResourceType for UpCloudManagedObjectStorageUserAccessKeyResource {
    fn schema(&self) -> &Schema {
        &self.schema
    }

    fn create(&self, ctx: &dyn OperationCtx, inputs: Value) -> Result<OperationResult, String> {
        let service_uuid = inputs
            .get("service_uuid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_user_access_key create: missing 'service_uuid'"
                    .to_string()
            })?;
        let username = inputs
            .get("username")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_user_access_key create: missing 'username'"
                    .to_string()
            })?;
        let desired_status = inputs
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("Active");

        let create_response = self.create_key(service_uuid, username)?;
        let access_key_id = create_response
            .get("access_key_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_user_access_key create: response missing \
                 'access_key_id'"
                    .to_string()
            })?
            .to_string();

        // Persist outputs as soon as the secret is known. The
        // `secret_access_key` is in `create_response` and will NEVER
        // appear again — it MUST land in saved outputs before we return,
        // otherwise a crash here strands an unrecoverable secret.
        ctx.save(&extract_outputs(&create_response, service_uuid, username));

        // If the user wants Inactive, flip with a PATCH. New keys come
        // up Active by default, so PATCH only fires when the desired
        // status differs from the API's default.
        let final_key = if desired_status != "Active" {
            let patched =
                self.patch_status(service_uuid, username, &access_key_id, desired_status)?;
            merge_secret_from(&patched, &create_response)
        } else {
            create_response
        };

        let outputs = extract_outputs(&final_key, service_uuid, username);
        Ok(OperationResult::Success { outputs })
    }

    fn read(&self, outputs: &Value) -> Result<OperationResult, String> {
        let service_uuid = outputs
            .get("service_uuid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_user_access_key read: missing 'service_uuid' \
                 in outputs"
                    .to_string()
            })?;
        let username = outputs
            .get("username")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_user_access_key read: missing 'username' in \
                 outputs"
                    .to_string()
            })?;
        let access_key_id = outputs
            .get("access_key_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_user_access_key read: missing 'access_key_id' \
                 in outputs"
                    .to_string()
            })?;
        match self.get_key(service_uuid, username, access_key_id)? {
            Some(key) => Ok(OperationResult::Success {
                outputs: extract_outputs(&key, service_uuid, username),
            }),
            None => Ok(OperationResult::NotFound),
        }
    }

    fn update(
        &self,
        _ctx: &dyn OperationCtx,
        _old_inputs: &Value,
        old_outputs: &Value,
        new_inputs: Value,
    ) -> Result<OperationResult, String> {
        let service_uuid = old_outputs
            .get("service_uuid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_user_access_key update: missing 'service_uuid' \
                 in old outputs"
                    .to_string()
            })?
            .to_string();
        let username = old_outputs
            .get("username")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_user_access_key update: missing 'username' in \
                 old outputs"
                    .to_string()
            })?
            .to_string();
        let access_key_id = old_outputs
            .get("access_key_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_user_access_key update: missing 'access_key_id' \
                 in old outputs"
                    .to_string()
            })?
            .to_string();
        let new_status = new_inputs
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("Active");

        let key = self.patch_status(&service_uuid, &username, &access_key_id, new_status)?;
        let outputs = extract_outputs(&key, &service_uuid, &username);
        // The PATCH response omits secret_access_key. The deploy layer's
        // preserve_secret_outputs will copy it forward from old_outputs
        // before persisting — we don't need to do it here.
        Ok(OperationResult::Success { outputs })
    }

    fn delete(&self, _ctx: &dyn OperationCtx, outputs: &Value) -> Result<OperationResult, String> {
        let service_uuid = outputs
            .get("service_uuid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_user_access_key delete: missing 'service_uuid' \
                 in outputs"
                    .to_string()
            })?;
        let username = outputs
            .get("username")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_user_access_key delete: missing 'username' in \
                 outputs"
                    .to_string()
            })?;
        let access_key_id = outputs
            .get("access_key_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_user_access_key delete: missing 'access_key_id' \
                 in outputs"
                    .to_string()
            })?;
        let path = format!(
            "/object-storage-2/{service_uuid}/users/{username}/access-keys/{access_key_id}"
        );
        let mut resp = self.client.delete(&path)?;
        let status = resp.status().as_u16();
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

    fn validate(&self, inputs: &Resolvable) -> Result<(), String> {
        let Some(inputs) = inputs.as_concrete() else {
            return Ok(());
        };
        if let Some(s) = inputs.get("status").and_then(|v| v.as_str()) {
            validate_status(s)?;
        }
        Ok(())
    }
}

fn validate_status(s: &str) -> Result<(), String> {
    match s {
        "Active" | "Inactive" => Ok(()),
        other => Err(format!(
            "status must be 'Active' or 'Inactive'; got '{other}'"
        )),
    }
}

/// Extract Blue outputs from an access-key API response. Adds
/// `service_uuid` and `username` mirrored from inputs (the API responses
/// don't echo either). Fields outside `OUTPUT_FIELDS` (notably
/// `last_used_at`) are deliberately dropped.
fn extract_outputs(key: &Value, service_uuid: &str, username: &str) -> Value {
    let mut out = Map::new();
    if let Some(obj) = key.as_object() {
        for &field in OUTPUT_FIELDS {
            if let Some(v) = obj.get(field) {
                out.insert(field.to_string(), v.clone());
            }
        }
    }
    out.insert(
        "service_uuid".to_string(),
        Value::String(service_uuid.to_string()),
    );
    out.insert(
        "username".to_string(),
        Value::String(username.to_string()),
    );
    Value::Object(out)
}

/// Copy `secret_access_key` from `original` into `patched` if `patched`
/// doesn't have it. The PATCH response omits the secret; the create
/// response is the only place it ever appeared, so we carry it forward
/// here when the user requested a non-default status.
fn merge_secret_from(patched: &Value, original: &Value) -> Value {
    let mut merged = patched.clone();
    let (Some(merged_obj), Some(orig_obj)) = (merged.as_object_mut(), original.as_object()) else {
        return merged;
    };
    if !merged_obj.contains_key("secret_access_key") {
        if let Some(secret) = orig_obj.get("secret_access_key") {
            merged_obj.insert("secret_access_key".to_string(), secret.clone());
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_parses() {
        let _ = crate::schema::parse_schema(SCHEMA).unwrap();
    }

    #[test]
    fn schema_marks_secret_access_key_secret() {
        let schema = crate::schema::parse_schema(SCHEMA).unwrap();
        let secret_field = schema
            .outputs
            .iter()
            .find(|o| o.path == "secret_access_key")
            .expect("secret_access_key output must exist");
        assert!(
            secret_field.secret,
            "secret_access_key MUST have secret = true; without it state encryption \
             won't apply and the secret will land cleartext"
        );
    }

    #[test]
    fn validate_status_accepts_active_and_inactive() {
        validate_status("Active").unwrap();
        validate_status("Inactive").unwrap();
    }

    #[test]
    fn validate_status_rejects_other() {
        let err = validate_status("active").unwrap_err(); // case-sensitive
        assert!(err.contains("Active"), "got: {err}");
        let err = validate_status("Disabled").unwrap_err();
        assert!(err.contains("'Disabled'"), "got: {err}");
    }

    #[test]
    fn extract_outputs_picks_declared_fields_and_mirrors_parents() {
        let key = json!({
            "access_key_id": "AKIA589142A152F5E423",
            "created_at": "2023-05-07T22:58:26.239729Z",
            "last_used_at": "2023-05-07T22:58:26.239729Z",
            "secret_access_key": "xbINHFALkXjFjmxhAYL8mJnODRDX91OM7cCd2+1Y",
            "status": "Active",
        });
        let outputs = extract_outputs(&key, "svc-uuid-123", "alice");
        let obj = outputs.as_object().unwrap();
        assert_eq!(obj["access_key_id"], "AKIA589142A152F5E423");
        assert_eq!(obj["secret_access_key"], "xbINHFALkXjFjmxhAYL8mJnODRDX91OM7cCd2+1Y");
        assert_eq!(obj["status"], "Active");
        assert_eq!(obj["created_at"], "2023-05-07T22:58:26.239729Z");
        assert_eq!(obj["service_uuid"], "svc-uuid-123");
        assert_eq!(obj["username"], "alice");
    }

    #[test]
    fn extract_outputs_omits_last_used_at() {
        // last_used_at would mutate on every S3 call, causing refresh
        // drift. Confirm we drop it even when present in the API response.
        let key = json!({
            "access_key_id": "AKIA1",
            "last_used_at": "2023-05-07T20:52:17Z",
            "status": "Active",
        });
        let outputs = extract_outputs(&key, "svc", "u");
        assert!(!outputs.as_object().unwrap().contains_key("last_used_at"));
    }

    #[test]
    fn extract_outputs_omits_secret_access_key_when_absent() {
        // GET responses don't include secret_access_key. Confirm we
        // simply omit the field rather than inserting null — the
        // preserve_secret_outputs hook at the deploy/refresh layer is
        // what carries it forward from prior state.
        let key = json!({
            "access_key_id": "AKIA1",
            "status": "Active",
            "created_at": "2023-05-07T22:58:26.239729Z",
        });
        let outputs = extract_outputs(&key, "svc", "u");
        assert!(!outputs
            .as_object()
            .unwrap()
            .contains_key("secret_access_key"));
    }
}
