use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use crate::provider::{OperationCtx, ResourceType};
use crate::types::{OperationResult, Schema};

use super::client::UpCloudClient;

const SCHEMA: &str = include_str!("schemas/upcloud_managed_object_storage_resource.toml");

/// Fields surfaced as Blue outputs (downstream resources can reference these
/// via `{{ resources.X.<field> }}`). `force_destroy` is mirrored separately
/// from inputs.
const OUTPUT_FIELDS: &[&str] = &[
    "uuid",
    "name",
    "region",
    "configured_status",
    "operational_state",
    "termination_protection",
    "created_at",
    "updated_at",
    "endpoints",
];

/// How long to wait between operational-state checks, and for how long total,
/// when polling a service to reach its target state after create or update.
const POLL_INTERVAL: Duration = Duration::from_secs(10);
const POLL_TIMEOUT: Duration = Duration::from_secs(600);

pub struct UpCloudManagedObjectStorageResource {
    schema: Schema,
    client: Arc<UpCloudClient>,
}

impl UpCloudManagedObjectStorageResource {
    pub fn new(client: Arc<UpCloudClient>) -> Self {
        let schema = crate::schema::parse_schema(SCHEMA)
            .expect("upcloud managed_object_storage resource schema must be valid");
        UpCloudManagedObjectStorageResource { schema, client }
    }

    fn create_service(&self, inputs: &Value) -> Result<Value, String> {
        let body = build_create_body(inputs);
        let path = "/object-storage-2";
        let mut resp = self.client.post(path, &body)?;
        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let err_body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(format!(
                "upcloud POST {path} failed: http status: {status}: {err_body}"
            ));
        }
        let service: Value = resp
            .body_mut()
            .read_json()
            .map_err(|e| format!("upcloud POST {path} response parse failed: {e}"))?;
        Ok(service)
    }

    fn modify_service(&self, uuid: &str, inputs: &Value) -> Result<Value, String> {
        let body = build_modify_body(inputs);
        let path = format!("/object-storage-2/{uuid}");
        let mut resp = self.client.patch(&path, &body)?;
        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let err_body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(format!(
                "upcloud PATCH {path} failed: http status: {status}: {err_body}"
            ));
        }
        let service: Value = resp
            .body_mut()
            .read_json()
            .map_err(|e| format!("upcloud PATCH {path} response parse failed: {e}"))?;
        Ok(service)
    }

    /// Fetch a single service by UUID. Returns Ok(None) on HTTP 404 so the
    /// caller can signal drift via OperationResult::NotFound.
    fn get_service(&self, uuid: &str) -> Result<Option<Value>, String> {
        let path = format!("/object-storage-2/{uuid}");
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
        let service: Value = resp
            .body_mut()
            .read_json()
            .map_err(|e| format!("upcloud GET {path} response parse failed: {e}"))?;
        Ok(Some(service))
    }

    /// Poll the service until `operational_state` matches the target derived
    /// from `configured_status`, or `POLL_TIMEOUT` elapses. Returns the latest
    /// service body. A 404 mid-poll is treated as an error — the service
    /// existed a moment ago, so vanishing means something went wrong.
    fn poll_until_target_state(
        &self,
        uuid: &str,
        configured_status: &str,
    ) -> Result<Value, String> {
        let target = target_operational_state(configured_status);
        let deadline = Instant::now() + POLL_TIMEOUT;
        loop {
            let service = self
                .get_service(uuid)?
                .ok_or_else(|| format!("upcloud service {uuid} disappeared while polling"))?;
            let state = service
                .get("operational_state")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if state == target {
                return Ok(service);
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "upcloud service {uuid} did not reach operational_state '{target}' within {}s; last state was '{state}'",
                    POLL_TIMEOUT.as_secs()
                ));
            }
            sleep(POLL_INTERVAL);
        }
    }
}

impl ResourceType for UpCloudManagedObjectStorageResource {
    fn schema(&self) -> &Schema {
        &self.schema
    }

    fn create(&self, ctx: &dyn OperationCtx, inputs: Value) -> Result<OperationResult, String> {
        let initial = self.create_service(&inputs)?;

        // Persist the uuid as soon as we have it, so a crash before this
        // function returns (e.g. during the long polling wait) doesn't
        // strand the service.
        let uuid = initial
            .get("uuid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                "upcloud.managed_object_storage create: response missing 'uuid'".to_string()
            })?
            .to_string();
        ctx.save(&json!({ "uuid": uuid }));

        let configured_status = inputs
            .get("configured_status")
            .and_then(|v| v.as_str())
            .unwrap_or("started");
        let service = self.poll_until_target_state(&uuid, configured_status)?;
        let outputs = extract_outputs(&service, inputs.get("force_destroy"));
        Ok(OperationResult::Success { outputs })
    }

    fn read(&self, outputs: &Value) -> Result<OperationResult, String> {
        let uuid = outputs.get("uuid").and_then(|v| v.as_str()).ok_or_else(|| {
            "upcloud.managed_object_storage read: missing 'uuid' in stored outputs".to_string()
        })?;
        match self.get_service(uuid)? {
            Some(service) => {
                // force_destroy isn't a UpCloud field — carry it forward from
                // the previously stored outputs so it survives refresh.
                let new_outputs = extract_outputs(&service, outputs.get("force_destroy"));
                Ok(OperationResult::Success {
                    outputs: new_outputs,
                })
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
                "upcloud.managed_object_storage update: missing 'uuid' in old outputs".to_string()
            })?
            .to_string();
        self.modify_service(&uuid, &new_inputs)?;

        // Poll on update too: a configured_status flip needs to settle before
        // returning, and for no-op state changes the first GET already matches.
        let configured_status = new_inputs
            .get("configured_status")
            .and_then(|v| v.as_str())
            .unwrap_or("started");
        let service = self.poll_until_target_state(&uuid, configured_status)?;
        let outputs = extract_outputs(&service, new_inputs.get("force_destroy"));
        Ok(OperationResult::Success { outputs })
    }

    fn delete(&self, _ctx: &dyn OperationCtx, outputs: &Value) -> Result<OperationResult, String> {
        let uuid = outputs.get("uuid").and_then(|v| v.as_str()).ok_or_else(|| {
            "upcloud.managed_object_storage delete: missing 'uuid' in stored outputs".to_string()
        })?;

        // Pre-flight: refuse to attempt the API call if our stored state says
        // termination_protection is on. UpCloud would return 400 SERVICE_ERROR
        // anyway; this fails faster with a clearer next step. If state is
        // stale (user disabled in the panel), `blue refresh` will pick it up.
        if outputs
            .get("termination_protection")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            return Err(format!(
                "upcloud.managed_object_storage {uuid}: termination_protection is enabled; \
                 set termination_protection = false and re-deploy before destroy \
                 (run `blue refresh` first if you disabled it manually)"
            ));
        }

        let force = outputs
            .get("force_destroy")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let path = if force {
            format!("/object-storage-2/{uuid}?force=true")
        } else {
            format!("/object-storage-2/{uuid}")
        };
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
        // Catch bad enum values at plan time. UpCloud will also validate, but
        // the field has only two valid values and we use it to pick a polling
        // target — better to fail fast.
        if let Some(s) = inputs.get("configured_status").and_then(|v| v.as_str()) {
            match s {
                "started" | "stopped" => {}
                other => {
                    return Err(format!(
                        "configured_status must be 'started' or 'stopped'; got '{other}'"
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Map a `configured_status` to the `operational_state` we expect to see once
/// the service has settled. UpCloud reports many transient states
/// (pending, setup-checkup, ...); only the two terminal ones below are valid
/// configured_status targets.
fn target_operational_state(configured_status: &str) -> &'static str {
    match configured_status {
        "stopped" => "stopped",
        _ => "running",
    }
}

/// Build the JSON body for `POST /object-storage-2`. Required fields (`name`,
/// `region`, `configured_status`) are guaranteed present by schema validation
/// + defaults; optional fields are only included if the user supplied them.
/// `force_destroy` is Blue-side and never goes to UpCloud.
fn build_create_body(inputs: &Value) -> Value {
    let mut body = Map::new();
    for field in [
        "name",
        "region",
        "configured_status",
        "termination_protection",
    ] {
        if let Some(v) = inputs.get(field) {
            body.insert(field.to_string(), v.clone());
        }
    }
    if let Some(networks) = inputs.get("networks").and_then(|v| v.as_array()) {
        if !networks.is_empty() {
            body.insert("networks".to_string(), Value::Array(networks.clone()));
        }
    }
    if let Some(labels) = inputs.get("labels").and_then(|v| v.as_array()) {
        if !labels.is_empty() {
            body.insert("labels".to_string(), Value::Array(labels.clone()));
        }
    }
    Value::Object(body)
}

/// Build the JSON Merge Patch body for `PATCH /object-storage-2/{uuid}`.
///
/// JSON Merge Patch semantics per UpCloud:
/// - absent property = leave unchanged
/// - explicit `null` = delete the property
/// - nested arrays are full-replace ("PUT within PATCH")
///
/// `name`, `configured_status`, and `termination_protection` are always
/// present in `inputs` (schema defaults), so we always include them.
///
/// `networks` and `labels` are always sent as arrays (possibly empty), so
/// removing them from config actually clears them on the service. Same
/// pattern as `backup_rule` on the block-storage resource.
///
/// `region` is force_new (a change produces Replace, not Update) and
/// `force_destroy` is Blue-side — neither is sent.
fn build_modify_body(inputs: &Value) -> Value {
    let mut body = Map::new();
    for field in ["name", "configured_status", "termination_protection"] {
        if let Some(v) = inputs.get(field) {
            body.insert(field.to_string(), v.clone());
        }
    }
    let networks = inputs
        .get("networks")
        .cloned()
        .unwrap_or_else(|| Value::Array(vec![]));
    body.insert("networks".to_string(), networks);
    let labels = inputs
        .get("labels")
        .cloned()
        .unwrap_or_else(|| Value::Array(vec![]));
    body.insert("labels".to_string(), labels);
    Value::Object(body)
}

/// Extract Blue outputs from a UpCloud service response. Only fields declared
/// in `OUTPUT_FIELDS` are exposed, plus the mirrored `force_destroy` value
/// carried from inputs (or its default).
fn extract_outputs(service: &Value, force_destroy: Option<&Value>) -> Value {
    let mut out = Map::new();
    if let Some(obj) = service.as_object() {
        for &field in OUTPUT_FIELDS {
            if let Some(v) = obj.get(field) {
                out.insert(field.to_string(), v.clone());
            }
        }
    }
    out.insert(
        "force_destroy".to_string(),
        force_destroy.cloned().unwrap_or(Value::Bool(false)),
    );
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn schema_parses() {
        let _ = crate::schema::parse_schema(SCHEMA).unwrap();
    }

    #[test]
    fn target_operational_state_maps_started_and_stopped() {
        assert_eq!(target_operational_state("started"), "running");
        assert_eq!(target_operational_state("stopped"), "stopped");
        // Defensive: any unexpected value falls through to running, matching
        // the schema default.
        assert_eq!(target_operational_state(""), "running");
    }

    #[test]
    fn build_create_body_minimal() {
        let inputs = json!({
            "name": "blue-mos",
            "region": "europe-1",
            "configured_status": "started",
            "termination_protection": false,
        });
        let body = build_create_body(&inputs);
        assert_eq!(
            body,
            json!({
                "name": "blue-mos",
                "region": "europe-1",
                "configured_status": "started",
                "termination_protection": false,
            })
        );
    }

    #[test]
    fn build_create_body_with_networks_and_labels() {
        let inputs = json!({
            "name": "blue-mos",
            "region": "europe-1",
            "configured_status": "started",
            "termination_protection": true,
            "networks": [
                {"name": "pub", "type": "public", "family": "IPv4"},
            ],
            "labels": [
                {"key": "env", "value": "prod"},
            ],
            "force_destroy": true,
        });
        let body = build_create_body(&inputs);
        let obj = body.as_object().unwrap();
        assert_eq!(obj["name"], "blue-mos");
        assert_eq!(obj["termination_protection"], true);
        assert_eq!(obj["networks"].as_array().unwrap().len(), 1);
        assert_eq!(obj["labels"].as_array().unwrap().len(), 1);
        // force_destroy is Blue-side; should NOT be sent to UpCloud
        assert!(!obj.contains_key("force_destroy"));
    }

    #[test]
    fn build_create_body_omits_empty_networks_and_labels() {
        let inputs = json!({
            "name": "blue-mos",
            "region": "europe-1",
            "configured_status": "started",
            "networks": [],
            "labels": [],
        });
        let body = build_create_body(&inputs);
        let obj = body.as_object().unwrap();
        assert!(!obj.contains_key("networks"));
        assert!(!obj.contains_key("labels"));
    }

    #[test]
    fn build_modify_body_includes_simple_fields_and_array_clears() {
        // User had networks and labels before, removed both from config.
        // Modify body should explicitly send empty arrays so UpCloud clears
        // them (rather than preserving them as it would for omitted fields
        // under merge-patch semantics).
        let inputs = json!({
            "name": "renamed",
            "region": "europe-1",         // force_new — must NOT appear in PATCH
            "configured_status": "stopped",
            "termination_protection": false,
            "force_destroy": true,        // Blue-side only — must NOT appear
        });
        let body = build_modify_body(&inputs);
        let obj = body.as_object().unwrap();
        assert_eq!(obj["name"], "renamed");
        assert_eq!(obj["configured_status"], "stopped");
        assert_eq!(obj["termination_protection"], false);
        assert_eq!(obj["networks"], json!([]));
        assert_eq!(obj["labels"], json!([]));
        assert!(!obj.contains_key("region"));
        assert!(!obj.contains_key("force_destroy"));
    }

    #[test]
    fn build_modify_body_passes_networks_and_labels_through() {
        let inputs = json!({
            "name": "x",
            "configured_status": "started",
            "termination_protection": false,
            "networks": [
                {"name": "pub", "type": "public", "family": "IPv4"},
            ],
            "labels": [
                {"key": "env", "value": "prod"},
            ],
        });
        let body = build_modify_body(&inputs);
        let obj = body.as_object().unwrap();
        assert_eq!(obj["networks"].as_array().unwrap().len(), 1);
        assert_eq!(obj["labels"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn extract_outputs_picks_declared_fields_plus_force_destroy() {
        let service = json!({
            "uuid": "1200ecde-db95-4d1c-9133-6508f3232567",
            "name": "example-service",
            "region": "europe-1",
            "configured_status": "started",
            "operational_state": "running",
            "termination_protection": false,
            "created_at": "2026-05-03T01:14:30.408125Z",
            "updated_at": "2026-05-03T01:14:30.408125Z",
            "endpoints": [
                {"domain_name": "x.upcloudobjects.com", "type": "public", "mode": "api"},
            ],
            "users": [],            // not exposed
            "custom_domains": [],   // not exposed
            "static_websites": [],  // not exposed
            "usage": {},            // not exposed
            "state_messages": [],   // not exposed
            "networks": [],         // not exposed (input-only)
            "labels": [],           // not exposed (input-only)
        });
        let outputs = extract_outputs(&service, Some(&json!(true)));
        let obj = outputs.as_object().unwrap();
        assert_eq!(obj.len(), 10); // 9 upcloud + force_destroy
        assert_eq!(obj["uuid"], "1200ecde-db95-4d1c-9133-6508f3232567");
        assert_eq!(obj["operational_state"], "running");
        assert_eq!(obj["force_destroy"], true);
        assert!(!obj.contains_key("users"));
        assert!(!obj.contains_key("usage"));
        assert!(!obj.contains_key("state_messages"));
        assert!(!obj.contains_key("networks"));
    }

    #[test]
    fn extract_outputs_defaults_force_destroy_to_false() {
        let service = json!({"uuid": "abc"});
        let outputs = extract_outputs(&service, None);
        assert_eq!(outputs.get("force_destroy").unwrap(), false);
    }
}
