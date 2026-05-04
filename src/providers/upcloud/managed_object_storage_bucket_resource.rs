use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use crate::provider::{OperationCtx, ResourceType};
use crate::resolvable::Resolvable;
use crate::types::{OperationResult, Schema};

use super::client::UpCloudClient;

const SCHEMA: &str =
    include_str!("schemas/upcloud_managed_object_storage_bucket_resource.toml");

/// Bucket fields surfaced as Blue outputs. `service_uuid` is added separately
/// (mirrored from inputs, since the API response doesn't include it).
const OUTPUT_FIELDS: &[&str] = &["name", "total_objects", "total_size_bytes"];

/// Page size for the list endpoint when refresh has to find a bucket by name.
/// The API caps `limit` at 100, so this is the largest single-call page.
const LIST_PAGE_SIZE: u32 = 100;

/// Poll cadence and timeout for waiting for a bucket to actually disappear
/// after DELETE. UpCloud's API returns 204 immediately but the bucket
/// remains visible (and its name reserved) during async cleanup —
/// without polling, an immediate same-name recreate hits
/// `400 "Bucket already exists"`.
const POLL_INTERVAL: Duration = Duration::from_secs(5);
const POLL_TIMEOUT: Duration = Duration::from_secs(300);

/// How many consecutive transport-level GET failures the polling loop
/// will tolerate before giving up. Cloud APIs occasionally drop
/// keep-alive connections (rustls close_notify); this absorbs that noise.
const POLL_MAX_CONSECUTIVE_ERRORS: u32 = 3;

pub struct UpCloudManagedObjectStorageBucketResource {
    schema: Schema,
    client: Arc<UpCloudClient>,
}

impl UpCloudManagedObjectStorageBucketResource {
    pub fn new(client: Arc<UpCloudClient>) -> Self {
        let schema = crate::schema::parse_schema(SCHEMA)
            .expect("upcloud managed_object_storage_bucket schema must be valid");
        UpCloudManagedObjectStorageBucketResource { schema, client }
    }

    fn create_bucket(&self, service_uuid: &str, name: &str) -> Result<Value, String> {
        let path = format!("/object-storage-2/{service_uuid}/buckets");
        let body = json!({ "name": name });
        let mut resp = self.client.post(&path, &body)?;
        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let err_body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(format!(
                "upcloud POST {path} failed: http status: {status}: {err_body}"
            ));
        }
        let bucket: Value = resp
            .body_mut()
            .read_json()
            .map_err(|e| format!("upcloud POST {path} response parse failed: {e}"))?;
        Ok(bucket)
    }

    /// Find a bucket by name within a service. The API has no GET-by-name —
    /// the only way to check existence is to walk the paginated list.
    /// Returns Ok(None) if the bucket isn't present, including the case where
    /// the parent service itself has been deleted (404 on the list endpoint).
    fn find_bucket(&self, service_uuid: &str, name: &str) -> Result<Option<Value>, String> {
        let mut offset: u32 = 0;
        loop {
            let path = format!(
                "/object-storage-2/{service_uuid}/buckets?limit={LIST_PAGE_SIZE}&offset={offset}"
            );
            let mut resp = self.client.get(&path)?;
            let status = resp.status().as_u16();
            // Parent service gone => bucket trivially gone too.
            if status == 404 {
                return Ok(None);
            }
            if !resp.status().is_success() {
                let err_body = resp.body_mut().read_to_string().unwrap_or_default();
                return Err(format!(
                    "upcloud GET {path} failed: http status: {status}: {err_body}"
                ));
            }
            let buckets: Vec<Value> = resp
                .body_mut()
                .read_json()
                .map_err(|e| format!("upcloud GET {path} response parse failed: {e}"))?;
            if buckets.is_empty() {
                return Ok(None);
            }
            for bucket in &buckets {
                if bucket.get("name").and_then(|v| v.as_str()) == Some(name) {
                    return Ok(Some(bucket.clone()));
                }
            }
            // Short page = last page; bucket isn't here.
            if (buckets.len() as u32) < LIST_PAGE_SIZE {
                return Ok(None);
            }
            offset += LIST_PAGE_SIZE;
        }
    }

    /// Poll the bucket list until our bucket is no longer present, or
    /// `POLL_TIMEOUT` elapses. Used by `delete` after the DELETE call
    /// returns success — UpCloud's API acknowledges the delete intent
    /// immediately (204) but the bucket remains visible during async
    /// cleanup, and an immediate same-name recreate hits
    /// `400 "Bucket already exists"`. Symmetric with the service
    /// resource's own `poll_until_gone`.
    ///
    /// Same transient-error tolerance as `poll_until_target_state`:
    /// up to `max_consecutive_errors` consecutive transport failures
    /// are absorbed before bailing out. A 404 on the parent service's
    /// list endpoint is treated as success (the parent went away too,
    /// which transitively means our bucket is gone).
    fn poll_until_gone(
        &self,
        service_uuid: &str,
        name: &str,
        max_consecutive_errors: u32,
    ) -> Result<(), String> {
        let deadline = Instant::now() + POLL_TIMEOUT;
        let mut consecutive_errors: u32 = 0;
        loop {
            match self.find_bucket(service_uuid, name) {
                Ok(None) => return Ok(()),
                Ok(Some(_)) => {
                    consecutive_errors = 0;
                    if Instant::now() >= deadline {
                        return Err(format!(
                            "upcloud bucket '{name}' did not finish deleting within {}s",
                            POLL_TIMEOUT.as_secs()
                        ));
                    }
                }
                Err(e) => {
                    consecutive_errors += 1;
                    if consecutive_errors >= max_consecutive_errors {
                        return Err(format!(
                            "upcloud bucket '{name}' delete-poll gave up after {consecutive_errors} consecutive transport error(s); last error: {e}"
                        ));
                    }
                    if Instant::now() >= deadline {
                        return Err(format!(
                            "upcloud bucket '{name}' delete-poll timed out after {}s; last error: {e}",
                            POLL_TIMEOUT.as_secs()
                        ));
                    }
                }
            }
            sleep(POLL_INTERVAL);
        }
    }
}

impl ResourceType for UpCloudManagedObjectStorageBucketResource {
    fn schema(&self) -> &Schema {
        &self.schema
    }

    fn create(&self, ctx: &dyn OperationCtx, inputs: Value) -> Result<OperationResult, String> {
        let service_uuid = inputs
            .get("service_uuid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_bucket create: missing 'service_uuid'".to_string()
            })?;
        let name = inputs
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_bucket create: missing 'name'".to_string()
            })?;
        let bucket = self.create_bucket(service_uuid, name)?;
        // Persist the identifying inputs as soon as the bucket exists, so a
        // crash before this function returns doesn't strand the resource —
        // refresh and delete both work off (service_uuid, name).
        ctx.save(&json!({ "service_uuid": service_uuid, "name": name }));
        let outputs = extract_outputs(&bucket, service_uuid);
        Ok(OperationResult::Success { outputs })
    }

    fn read(&self, outputs: &Value) -> Result<OperationResult, String> {
        let service_uuid = outputs
            .get("service_uuid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_bucket read: missing 'service_uuid' in outputs"
                    .to_string()
            })?;
        let name = outputs
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_bucket read: missing 'name' in outputs".to_string()
            })?;
        match self.find_bucket(service_uuid, name)? {
            Some(bucket) => Ok(OperationResult::Success {
                outputs: extract_outputs(&bucket, service_uuid),
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
        // Both inputs (`service_uuid`, `name`) are force_new and the bucket
        // API has no PATCH/PUT, so the diff engine should always produce
        // Replace (or Create/Delete) for any change. Reaching this method
        // would indicate a planning bug — fail loudly instead of pretending.
        Err(
            "upcloud.managed_object_storage_bucket: update should never be called — \
             all inputs are force_new"
                .to_string(),
        )
    }

    fn delete(&self, _ctx: &dyn OperationCtx, outputs: &Value) -> Result<OperationResult, String> {
        let service_uuid = outputs
            .get("service_uuid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_bucket delete: missing 'service_uuid' in outputs"
                    .to_string()
            })?;
        let name = outputs
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage_bucket delete: missing 'name' in outputs"
                    .to_string()
            })?;
        let path = format!("/object-storage-2/{service_uuid}/buckets/{name}");
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

        // DELETE returns 204 immediately, but UpCloud finishes the bucket
        // teardown asynchronously and keeps the name reserved during
        // cleanup. Poll the list until the bucket is no longer present so
        // an immediate same-name recreate (Replace flows where a bucket
        // is deleted then recreated) doesn't hit
        // `400 "Bucket already exists"`.
        self.poll_until_gone(service_uuid, name, POLL_MAX_CONSECUTIVE_ERRORS)?;

        Ok(OperationResult::Success { outputs: json!({}) })
    }

    fn validate(&self, inputs: &Resolvable) -> Result<(), String> {
        // Only validate when fully concrete — pending refs get re-checked
        // at deploy time after strict resolution. Doing it on the concrete
        // value is also load-bearing: the bucket name eventually lands in
        // a URL, and we want the path-traversal check to run on the real
        // value, not a `{{ ... }}` template.
        let Some(inputs) = inputs.as_concrete() else {
            return Ok(());
        };
        // API rule: 1-254 chars from [a-zA-Z0-9._-]. Validating client-side
        // catches typos at plan time, and also rules out path-traversal
        // characters ('/', '..') that would otherwise be interpolated raw
        // into the URL we construct in create/delete/find_bucket.
        if let Some(name) = inputs.get("name").and_then(|v| v.as_str()) {
            validate_bucket_name(name)?;
        }
        Ok(())
    }
}

fn validate_bucket_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 254 {
        return Err(format!(
            "name must be 1-254 characters; got {} chars",
            name.len()
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
    {
        return Err(format!(
            "name must contain only alphanumeric, '.', '-', and '_'; got '{name}'"
        ));
    }
    Ok(())
}

/// Extract Blue outputs from a bucket API response. Only fields declared in
/// `OUTPUT_FIELDS` are exposed, plus `service_uuid` mirrored from inputs
/// (the bucket response itself doesn't echo the parent UUID).
fn extract_outputs(bucket: &Value, service_uuid: &str) -> Value {
    let mut out = Map::new();
    if let Some(obj) = bucket.as_object() {
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
    fn validate_bucket_name_accepts_allowed_characters() {
        validate_bucket_name("simple").unwrap();
        validate_bucket_name("with-hyphens").unwrap();
        validate_bucket_name("with_underscores").unwrap();
        validate_bucket_name("with.dots").unwrap();
        validate_bucket_name("Mixed.123_with-everything").unwrap();
        validate_bucket_name("a").unwrap(); // 1-char minimum
        validate_bucket_name(&"x".repeat(254)).unwrap(); // max length
    }

    #[test]
    fn validate_bucket_name_rejects_empty_and_too_long() {
        let err = validate_bucket_name("").unwrap_err();
        assert!(err.contains("1-254"), "got: {err}");
        let err = validate_bucket_name(&"x".repeat(255)).unwrap_err();
        assert!(err.contains("1-254"), "got: {err}");
    }

    #[test]
    fn validate_bucket_name_rejects_path_traversal_characters() {
        // Defense-in-depth: the name lands in URL paths in create/delete/find,
        // so '/' and '..' must be rejected here even though UpCloud would
        // also reject them.
        for bad in ["foo/bar", "../foo", "with space", "uni©de", "with#hash"] {
            assert!(
                validate_bucket_name(bad).is_err(),
                "expected '{bad}' to be rejected"
            );
        }
    }

    #[test]
    fn extract_outputs_picks_declared_fields_and_mirrors_service_uuid() {
        let bucket = json!({
            "name": "my-bucket-1",
            "total_objects": 157,
            "total_size_bytes": 5293771,
            // Hypothetical extra fields should be ignored.
            "internal_id": "x",
        });
        let outputs = extract_outputs(&bucket, "12bbc828-a18a-42b8-af04-8d8554dc1e17");
        let obj = outputs.as_object().unwrap();
        assert_eq!(obj.len(), 4);
        assert_eq!(obj["name"], "my-bucket-1");
        assert_eq!(obj["total_objects"], 157);
        assert_eq!(obj["total_size_bytes"], 5293771);
        assert_eq!(obj["service_uuid"], "12bbc828-a18a-42b8-af04-8d8554dc1e17");
        assert!(!obj.contains_key("internal_id"));
    }

    #[test]
    fn extract_outputs_omits_missing_fields() {
        // Defensive: if the API ever returns a sparse body (e.g. just-created
        // bucket without metrics), missing fields are simply not present in
        // outputs rather than null.
        let bucket = json!({"name": "fresh"});
        let outputs = extract_outputs(&bucket, "svc-uuid");
        let obj = outputs.as_object().unwrap();
        assert_eq!(obj["name"], "fresh");
        assert_eq!(obj["service_uuid"], "svc-uuid");
        assert!(!obj.contains_key("total_objects"));
        assert!(!obj.contains_key("total_size_bytes"));
    }
}
