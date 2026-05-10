use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use crate::provider::{OperationCtx, ResourceType};
use crate::resolvable::Resolvable;
use crate::types::{OperationResult, Schema};

use super::client::UpCloudClient;

const SCHEMA: &str = include_str!("schemas/upcloud_managed_object_storage_user_resource.toml");

/// User fields surfaced as Blue outputs. `service_uuid` is added separately
/// (mirrored from inputs, since the API response doesn't include it).
/// `access_keys` and `policies` are intentionally excluded — they're owned by
/// separate APIs and would cause refresh-time drift if surfaced here.
const OUTPUT_FIELDS: &[&str] = &["username", "arn", "created_at"];

/// Username reserved by UpCloud for internal use; rejected at plan time.
const RESERVED_USERNAME: &str = "_upcloud-internal-user";

/// Poll cadence and timeout for waiting for a user to actually disappear
/// after DELETE. Symmetric with the bucket resource: UpCloud's API may
/// finish deletes asynchronously, so an immediate same-name recreate
/// (Replace flow) could otherwise race the cleanup.
const POLL_INTERVAL: Duration = Duration::from_secs(5);
const POLL_TIMEOUT: Duration = Duration::from_secs(300);

/// How many consecutive transport-level GET failures the polling loop
/// will tolerate before giving up. Cloud APIs occasionally drop
/// keep-alive connections (rustls close_notify); this absorbs that noise.
const POLL_MAX_CONSECUTIVE_ERRORS: u32 = 3;

pub struct UpCloudManagedObjectStorageUserResource {
    schema: Schema,
    client: Arc<UpCloudClient>,
}

impl UpCloudManagedObjectStorageUserResource {
    pub fn new(client: Arc<UpCloudClient>) -> Self {
        let schema = crate::schema::parse_schema(SCHEMA)
            .expect("upcloud managed_object_storage_user schema must be valid");
        UpCloudManagedObjectStorageUserResource { schema, client }
    }

    fn create_user(&self, service_uuid: &str, username: &str) -> Result<Value, String> {
        let path = format!("/object-storage-2/{service_uuid}/users");
        let body = json!({ "username": username });
        let mut resp = self.client.post(&path, &body)?;
        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let err_body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(format!(
                "upcloud POST {path} failed: http status: {status}: {err_body}"
            ));
        }
        let user: Value = resp
            .body_mut()
            .read_json()
            .map_err(|e| format!("upcloud POST {path} response parse failed: {e}"))?;
        Ok(user)
    }

    /// Fetch a single user by (service_uuid, username). Returns Ok(None) on
    /// HTTP 404 — covering both "user gone" and "parent service gone".
    fn get_user(&self, service_uuid: &str, username: &str) -> Result<Option<Value>, String> {
        let path = format!("/object-storage-2/{service_uuid}/users/{username}");
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
        let user: Value = resp
            .body_mut()
            .read_json()
            .map_err(|e| format!("upcloud GET {path} response parse failed: {e}"))?;
        Ok(Some(user))
    }

    /// Poll the user GET endpoint until it returns 404, or `POLL_TIMEOUT`
    /// elapses. Used by `delete` after the DELETE call returns success —
    /// matches the safety net the bucket and service resources have for
    /// async cleanup races on Replace flows.
    ///
    /// Up to `max_consecutive_errors` consecutive transport failures are
    /// absorbed before bailing out; the counter resets on every successful
    /// response.
    fn poll_until_gone(
        &self,
        service_uuid: &str,
        username: &str,
        max_consecutive_errors: u32,
    ) -> Result<(), String> {
        let deadline = Instant::now() + POLL_TIMEOUT;
        let mut consecutive_errors: u32 = 0;
        loop {
            match self.get_user(service_uuid, username) {
                Ok(None) => return Ok(()),
                Ok(Some(_)) => {
                    consecutive_errors = 0;
                    if Instant::now() >= deadline {
                        return Err(format!(
                            "upcloud user '{username}' did not finish deleting within {}s",
                            POLL_TIMEOUT.as_secs()
                        ));
                    }
                }
                Err(e) => {
                    consecutive_errors += 1;
                    if consecutive_errors >= max_consecutive_errors {
                        return Err(format!(
                            "upcloud user '{username}' delete-poll gave up after {consecutive_errors} consecutive transport error(s); last error: {e}"
                        ));
                    }
                    if Instant::now() >= deadline {
                        return Err(format!(
                            "upcloud user '{username}' delete-poll timed out after {}s; last error: {e}",
                            POLL_TIMEOUT.as_secs()
                        ));
                    }
                }
            }
            sleep(POLL_INTERVAL);
        }
    }
}

impl ResourceType for UpCloudManagedObjectStorageUserResource {
    fn schema(&self) -> &Schema {
        &self.schema
    }

    fn create(&self, ctx: &dyn OperationCtx, inputs: Value) -> Result<OperationResult, String> {
        let service_uuid = inputs
            .get("service_uuid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_user create: missing 'service_uuid'".to_string()
            })?;
        let username = inputs
            .get("username")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_user create: missing 'username'".to_string()
            })?;
        let user = self.create_user(service_uuid, username)?;
        // Persist the identifying inputs as soon as the user exists, so a
        // crash before this function returns doesn't strand the resource —
        // refresh and delete both work off (service_uuid, username).
        ctx.save(&json!({ "service_uuid": service_uuid, "username": username }));

        // The POST response doesn't include `arn`, but a follow-up GET does.
        // Refresh once so the saved outputs are complete from the start —
        // otherwise the first `read` after create would change them.
        let outputs = match self.get_user(service_uuid, username)? {
            Some(full) => extract_outputs(&full, service_uuid),
            None => extract_outputs(&user, service_uuid),
        };
        Ok(OperationResult::Success { outputs })
    }

    fn read(&self, outputs: &Value) -> Result<OperationResult, String> {
        let service_uuid = outputs
            .get("service_uuid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_user read: missing 'service_uuid' in outputs"
                    .to_string()
            })?;
        let username = outputs
            .get("username")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_user read: missing 'username' in outputs"
                    .to_string()
            })?;
        match self.get_user(service_uuid, username)? {
            Some(user) => Ok(OperationResult::Success {
                outputs: extract_outputs(&user, service_uuid),
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
        // Both inputs (`service_uuid`, `username`) are force_new and the user
        // API has no PATCH/PUT, so the diff engine should always produce
        // Replace (or Create/Delete) for any change. Reaching this method
        // would indicate a planning bug — fail loudly instead of pretending.
        Err(
            "upcloud.managed_object_storage_user: update should never be called — \
             all inputs are force_new"
                .to_string(),
        )
    }

    fn delete(&self, _ctx: &dyn OperationCtx, outputs: &Value) -> Result<OperationResult, String> {
        let service_uuid = outputs
            .get("service_uuid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_user delete: missing 'service_uuid' in outputs"
                    .to_string()
            })?;
        let username = outputs
            .get("username")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_user delete: missing 'username' in outputs"
                    .to_string()
            })?;
        let path = format!("/object-storage-2/{service_uuid}/users/{username}");
        let mut resp = self.client.delete(&path)?;
        let status = resp.status().as_u16();
        // 404 = already gone; treat as idempotent success. No need to poll.
        if status == 404 {
            return Ok(OperationResult::Success { outputs: json!({}) });
        }
        if !resp.status().is_success() {
            let err_body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(format!(
                "upcloud DELETE {path} failed: http status: {status}: {err_body}"
            ));
        }

        // DELETE returns 204 immediately; poll until the user is no longer
        // visible so a same-name recreate (Replace flows) doesn't race async
        // cleanup. Symmetric with the bucket and service resources.
        self.poll_until_gone(service_uuid, username, POLL_MAX_CONSECUTIVE_ERRORS)?;

        Ok(OperationResult::Success { outputs: json!({}) })
    }

    fn validate(&self, inputs: &Resolvable) -> Result<(), String> {
        // Only validate when fully concrete — pending refs get re-checked
        // at deploy time after strict resolution. Doing it on the concrete
        // value is also load-bearing: the username eventually lands in a
        // URL, and we want the path-traversal check to run on the real
        // value, not a `{{ ... }}` template.
        let Some(inputs) = inputs.as_concrete() else {
            return Ok(());
        };
        if let Some(username) = inputs.get("username").and_then(|v| v.as_str()) {
            validate_username(username)?;
        }
        Ok(())
    }
}

/// Validate a service-user `username` against UpCloud's spec:
/// 1-64 chars from `[\w+=,.@-]+` (alphanumerics, underscore, '+', '=', ',',
/// '.', '@', '-'). Also rejects the reserved `_upcloud-internal-user`.
///
/// Validating client-side catches typos at plan time, and rules out path-
/// traversal characters ('/', '..') that would otherwise be interpolated
/// raw into the URL we construct in create/get/delete.
fn validate_username(username: &str) -> Result<(), String> {
    if username == RESERVED_USERNAME {
        return Err(format!(
            "username '{RESERVED_USERNAME}' is reserved by UpCloud and cannot be used"
        ));
    }
    if username.is_empty() || username.len() > 64 {
        return Err(format!(
            "username must be 1-64 characters; got {} chars",
            username.len()
        ));
    }
    if !username.chars().all(is_allowed_username_char) {
        return Err(format!(
            "username must contain only alphanumerics, '_', '+', '=', ',', '.', '@', and '-'; got '{username}'"
        ));
    }
    Ok(())
}

/// Match the UpCloud regex `[\w+=,.@-]`. `\w` in PCRE is `[A-Za-z0-9_]`.
fn is_allowed_username_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || c == '_'
        || c == '+'
        || c == '='
        || c == ','
        || c == '.'
        || c == '@'
        || c == '-'
}

/// Extract Blue outputs from a user API response. Only fields declared in
/// `OUTPUT_FIELDS` are exposed, plus `service_uuid` mirrored from inputs
/// (the user response itself doesn't echo the parent UUID).
fn extract_outputs(user: &Value, service_uuid: &str) -> Value {
    let mut out = Map::new();
    if let Some(obj) = user.as_object() {
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
    fn validate_username_accepts_allowed_characters() {
        validate_username("simple").unwrap();
        validate_username("with-hyphens").unwrap();
        validate_username("with_underscore").unwrap();
        validate_username("with.dots").unwrap();
        validate_username("with+plus").unwrap();
        validate_username("with=equal").unwrap();
        validate_username("with,comma").unwrap();
        validate_username("user@example.com").unwrap();
        validate_username("Mixed.123_with-everything+,=@").unwrap();
        validate_username("a").unwrap(); // 1-char minimum
        validate_username(&"x".repeat(64)).unwrap(); // max length
    }

    #[test]
    fn validate_username_rejects_empty_and_too_long() {
        let err = validate_username("").unwrap_err();
        assert!(err.contains("1-64"), "got: {err}");
        let err = validate_username(&"x".repeat(65)).unwrap_err();
        assert!(err.contains("1-64"), "got: {err}");
    }

    #[test]
    fn validate_username_rejects_disallowed_characters() {
        // Defense-in-depth: the username lands in URL paths in
        // create/get/delete, so '/' and '..' must be rejected here even
        // though UpCloud would also reject them. Spaces and non-ASCII are
        // outside the API's regex and must be rejected too.
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
                validate_username(bad).is_err(),
                "expected '{bad}' to be rejected"
            );
        }
    }

    #[test]
    fn validate_username_rejects_reserved_name() {
        let err = validate_username(RESERVED_USERNAME).unwrap_err();
        assert!(err.contains("reserved"), "got: {err}");
        assert!(err.contains(RESERVED_USERNAME), "got: {err}");
    }

    #[test]
    fn extract_outputs_picks_declared_fields_and_mirrors_service_uuid() {
        let user = json!({
            "username": "example_user",
            "arn": "urn:ecs:iam::123bbb5c6a4240409e07f7d89fe28891:user/example_user",
            "created_at": "2023-05-07T15:55:24.655776Z",
            // Not surfaced — owned by sibling resources.
            "access_keys": [
                {"access_key_id": "AKIA63F41D01345BB477", "status": "Active"},
            ],
            "policies": [
                {"arn": "urn:ecs:iam:::policy/ECSS3FullAccess", "name": "ECSS3FullAccess"},
            ],
        });
        let outputs = extract_outputs(&user, "12bbc828-a18a-42b8-af04-8d8554dc1e17");
        let obj = outputs.as_object().unwrap();
        assert_eq!(obj.len(), 4);
        assert_eq!(obj["username"], "example_user");
        assert_eq!(
            obj["arn"],
            "urn:ecs:iam::123bbb5c6a4240409e07f7d89fe28891:user/example_user"
        );
        assert_eq!(obj["created_at"], "2023-05-07T15:55:24.655776Z");
        assert_eq!(obj["service_uuid"], "12bbc828-a18a-42b8-af04-8d8554dc1e17");
        assert!(!obj.contains_key("access_keys"));
        assert!(!obj.contains_key("policies"));
    }

    #[test]
    fn extract_outputs_omits_missing_fields() {
        // POST returns a sparse body without `arn`; outputs should simply
        // omit it rather than carry a null. The follow-up GET fills it in.
        let user = json!({
            "username": "example_user",
            "created_at": "2023-05-07T18:19:16.507681Z",
            "access_keys": [],
            "policies": [],
        });
        let outputs = extract_outputs(&user, "svc-uuid");
        let obj = outputs.as_object().unwrap();
        assert_eq!(obj["username"], "example_user");
        assert_eq!(obj["service_uuid"], "svc-uuid");
        assert!(!obj.contains_key("arn"));
    }
}
