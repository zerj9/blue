use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::provider::{OperationCtx, ResourceType};
use crate::resolvable::Resolvable;
use crate::types::{OperationResult, Schema};

use super::client::UpCloudClient;

const SCHEMA: &str =
    include_str!("schemas/upcloud_managed_object_storage_user_policy_resource.toml");

pub struct UpCloudManagedObjectStorageUserPolicyResource {
    schema: Schema,
    client: Arc<UpCloudClient>,
}

impl UpCloudManagedObjectStorageUserPolicyResource {
    pub fn new(client: Arc<UpCloudClient>) -> Self {
        let schema = crate::schema::parse_schema(SCHEMA)
            .expect("upcloud managed_object_storage_user_policy schema must be valid");
        UpCloudManagedObjectStorageUserPolicyResource { schema, client }
    }

    fn attach_policy(
        &self,
        service_uuid: &str,
        username: &str,
        policy_name: &str,
    ) -> Result<(), String> {
        let path = format!("/object-storage-2/{service_uuid}/users/{username}/policies");
        let body = json!({ "name": policy_name });
        let mut resp = self.client.post(&path, &body)?;
        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let err_body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(format!(
                "upcloud POST {path} failed: http status: {status}: {err_body}"
            ));
        }
        Ok(())
    }

    /// List the policies attached to a user and return the entry whose
    /// `name` matches `policy_name`. `Ok(None)` covers both:
    /// - the policy is not attached, and
    /// - the parent user or service is gone (404 on the LIST).
    fn find_attachment(
        &self,
        service_uuid: &str,
        username: &str,
        policy_name: &str,
    ) -> Result<Option<Value>, String> {
        let path = format!("/object-storage-2/{service_uuid}/users/{username}/policies");
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
        let listing: Value = resp
            .body_mut()
            .read_json()
            .map_err(|e| format!("upcloud GET {path} response parse failed: {e}"))?;
        let entries = listing
            .as_array()
            .ok_or_else(|| format!("upcloud GET {path} expected JSON array; got: {listing}"))?;
        Ok(entries
            .iter()
            .find(|entry| {
                entry
                    .get("name")
                    .and_then(|v| v.as_str())
                    .is_some_and(|n| n == policy_name)
            })
            .cloned())
    }
}

impl ResourceType for UpCloudManagedObjectStorageUserPolicyResource {
    fn schema(&self) -> &Schema {
        &self.schema
    }

    fn create(&self, ctx: &dyn OperationCtx, inputs: Value) -> Result<OperationResult, String> {
        let service_uuid = inputs
            .get("service_uuid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_user_policy create: missing 'service_uuid'"
                    .to_string()
            })?;
        let username = inputs
            .get("username")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_user_policy create: missing 'username'".to_string()
            })?;
        let policy_name = inputs
            .get("policy_name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_user_policy create: missing 'policy_name'"
                    .to_string()
            })?;

        self.attach_policy(service_uuid, username, policy_name)?;

        // Persist the identifying inputs as soon as the attachment exists, so
        // a crash during the follow-up GET-list doesn't strand the resource —
        // refresh and delete both work off (service_uuid, username, policy_name).
        ctx.save(&json!({
            "service_uuid": service_uuid,
            "username": username,
            "policy_name": policy_name,
        }));

        // The POST returns no body; fetch the matching entry from the LIST so
        // `arn` is populated from the start. If the list call doesn't show
        // the just-attached policy (transient consistency), fall back to
        // outputs without `arn` — refresh will fill it in next run.
        let outputs = match self.find_attachment(service_uuid, username, policy_name)? {
            Some(entry) => extract_outputs(&entry, service_uuid, username, policy_name),
            None => extract_outputs(&Value::Null, service_uuid, username, policy_name),
        };
        Ok(OperationResult::Success { outputs })
    }

    fn read(&self, outputs: &Value) -> Result<OperationResult, String> {
        let service_uuid = outputs
            .get("service_uuid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_user_policy read: missing 'service_uuid' in outputs"
                    .to_string()
            })?;
        let username = outputs
            .get("username")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_user_policy read: missing 'username' in outputs"
                    .to_string()
            })?;
        let policy_name = outputs
            .get("policy_name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_user_policy read: missing 'policy_name' in outputs"
                    .to_string()
            })?;
        match self.find_attachment(service_uuid, username, policy_name)? {
            Some(entry) => Ok(OperationResult::Success {
                outputs: extract_outputs(&entry, service_uuid, username, policy_name),
            }),
            None => Ok(OperationResult::NotFound),
        }
    }

    fn update(
        &self,
        _ctx: &dyn OperationCtx,
        _old_inputs: &Value,
        _old_outputs: &Value,
        _new_inputs: Value,
    ) -> Result<OperationResult, String> {
        // All inputs are force_new and there is no PATCH endpoint, so any
        // change always produces Replace (or Create/Delete). Reaching this
        // method indicates a planning bug — fail loudly.
        Err(
            "upcloud.managed_object_storage_user_policy: update should never be called — \
             all inputs are force_new"
                .to_string(),
        )
    }

    fn delete(&self, _ctx: &dyn OperationCtx, outputs: &Value) -> Result<OperationResult, String> {
        let service_uuid = outputs
            .get("service_uuid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_user_policy delete: missing 'service_uuid' in outputs"
                    .to_string()
            })?;
        let username = outputs
            .get("username")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_user_policy delete: missing 'username' in outputs"
                    .to_string()
            })?;
        let policy_name = outputs
            .get("policy_name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_user_policy delete: missing 'policy_name' in outputs"
                    .to_string()
            })?;
        let path =
            format!("/object-storage-2/{service_uuid}/users/{username}/policies/{policy_name}");
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
        if let Some(name) = inputs.get("policy_name").and_then(|v| v.as_str()) {
            validate_policy_name(name)?;
        }
        Ok(())
    }
}

/// Reject empty names and any name containing characters that would break
/// the URL we construct in `delete`. UpCloud's policy names in practice are
/// camel-case identifiers like `ECSS3FullAccess`; a strict-but-broad
/// allowlist catches typos and rules out path-traversal at plan time.
fn validate_policy_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("policy_name must not be empty".to_string());
    }
    if name.len() > 128 {
        return Err(format!(
            "policy_name must be 1-128 characters; got {} chars",
            name.len()
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '+' || c == '.')
    {
        return Err(format!(
            "policy_name must contain only alphanumerics, '_', '-', '+', and '.'; got '{name}'"
        ));
    }
    Ok(())
}

/// Build outputs by combining the listing entry (for `arn`) with the
/// identifying inputs (mirrored so refresh/delete can find their target
/// even if the listing endpoint stops echoing them in the future).
fn extract_outputs(entry: &Value, service_uuid: &str, username: &str, policy_name: &str) -> Value {
    let mut out = Map::new();
    if let Some(obj) = entry.as_object() {
        if let Some(v) = obj.get("arn") {
            out.insert("arn".to_string(), v.clone());
        }
    }
    out.insert(
        "service_uuid".to_string(),
        Value::String(service_uuid.to_string()),
    );
    out.insert("username".to_string(), Value::String(username.to_string()));
    out.insert(
        "policy_name".to_string(),
        Value::String(policy_name.to_string()),
    );
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_parses() {
        let _ = crate::schema::parse_schema(SCHEMA).unwrap();
    }

    #[test]
    fn validate_policy_name_accepts_typical_names() {
        validate_policy_name("ECSS3FullAccess").unwrap();
        validate_policy_name("IAMFullAccess").unwrap();
        validate_policy_name("with-dashes").unwrap();
        validate_policy_name("with_underscore").unwrap();
        validate_policy_name("with.dots").unwrap();
        validate_policy_name("a").unwrap();
        validate_policy_name(&"x".repeat(128)).unwrap();
    }

    #[test]
    fn validate_policy_name_rejects_empty_and_too_long() {
        let err = validate_policy_name("").unwrap_err();
        assert!(err.contains("empty"), "got: {err}");
        let err = validate_policy_name(&"x".repeat(129)).unwrap_err();
        assert!(err.contains("1-128"), "got: {err}");
    }

    #[test]
    fn validate_policy_name_rejects_path_traversal_and_disallowed_chars() {
        for bad in [
            "foo/bar",
            "../foo",
            "with space",
            "uni©de",
            "with#hash",
            "with*star",
            "with(paren)",
        ] {
            assert!(
                validate_policy_name(bad).is_err(),
                "expected '{bad}' to be rejected"
            );
        }
    }

    #[test]
    fn extract_outputs_picks_arn_and_mirrors_inputs() {
        let entry = json!({
            "arn": "urn:ecs:iam:::policy/ECSS3FullAccess",
            "name": "ECSS3FullAccess",
        });
        let outputs = extract_outputs(&entry, "svc-uuid-123", "alice", "ECSS3FullAccess");
        let obj = outputs.as_object().unwrap();
        assert_eq!(obj["arn"], "urn:ecs:iam:::policy/ECSS3FullAccess");
        assert_eq!(obj["service_uuid"], "svc-uuid-123");
        assert_eq!(obj["username"], "alice");
        assert_eq!(obj["policy_name"], "ECSS3FullAccess");
    }

    #[test]
    fn extract_outputs_omits_arn_when_absent() {
        // Fallback path: POST succeeded but the LIST didn't echo the new
        // attachment yet. We surface the identifying fields without `arn`;
        // the next refresh fills it in.
        let outputs = extract_outputs(&Value::Null, "svc", "u", "ECSS3FullAccess");
        let obj = outputs.as_object().unwrap();
        assert!(!obj.contains_key("arn"));
        assert_eq!(obj["service_uuid"], "svc");
        assert_eq!(obj["username"], "u");
        assert_eq!(obj["policy_name"], "ECSS3FullAccess");
    }
}
