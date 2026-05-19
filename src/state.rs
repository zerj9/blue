use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::crypto;
use crate::types::Schema;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub lineage: String,
    pub serial: u64,
    /// Sorted list of recipient strings used at the most recent successful
    /// `write_state`. Empty when no encryption was applied. Compared
    /// against the current config's recipients at deploy entry to detect
    /// drift (which requires `blue rekey` before proceeding).
    #[serde(default)]
    pub encrypted_with: Vec<String>,
    #[serde(default)]
    pub resources: HashMap<String, ResourceState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceState {
    #[serde(rename = "type")]
    pub resource_type: String,
    pub inputs: Value,
    pub outputs: Value,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

impl State {
    pub fn new() -> Self {
        State {
            lineage: Uuid::new_v4().to_string(),
            serial: 0,
            encrypted_with: Vec::new(),
            resources: HashMap::new(),
        }
    }
}

/// Schema lookup interface. Implemented by `Providers` (in `provider.rs`)
/// for the production path; `NoSchemas` is the test/zero-deps fallback.
/// Decoupled into a trait so `state.rs` doesn't depend on `provider.rs`.
pub trait SchemaResolver {
    fn schema(&self, type_name: &str) -> Option<&Schema>;
}

/// No-op resolver. Used by `StateIO::plaintext()` and by tests that don't
/// care about encryption — `write_state` skips encrypting any resource
/// because no schema is found, so secret-flag handling is short-circuited.
pub struct NoSchemas;

impl SchemaResolver for NoSchemas {
    fn schema(&self, _: &str) -> Option<&Schema> {
        None
    }
}

/// Cross-cutting params for state read/write. Held by reference so the
/// caller (CLI command handlers) own the underlying recipient/identity
/// vectors and the schema resolver. The `'a` lifetime is the longest
/// borrow we need; in practice it's the lifetime of `Providers` and the
/// loaded recipients/identities at the top of each command.
pub struct StateIO<'a> {
    pub recipients: &'a [age::x25519::Recipient],
    pub identities: &'a [Box<dyn age::Identity>],
    pub recipients_raw: &'a [String],
    pub schemas: &'a dyn SchemaResolver,
}

static NO_SCHEMAS: NoSchemas = NoSchemas;

impl StateIO<'static> {
    /// StateIO with no recipients, no identities, and a no-op schema
    /// resolver. Encrypts nothing on write; on read, it errors if any
    /// encrypted markers are encountered (because no identity is
    /// available to decrypt them). Used by tests and by paths that
    /// genuinely need plaintext-only handling.
    pub fn plaintext() -> Self {
        StateIO {
            recipients: &[],
            identities: &[],
            recipients_raw: &[],
            schemas: &NO_SCHEMAS,
        }
    }
}

pub fn read_state(path: &Path, io: &StateIO) -> Result<State, String> {
    if !path.exists() {
        return Ok(State::new());
    }
    let contents = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read state file {}: {e}", path.display()))?;
    let mut state: State = serde_json::from_str(&contents)
        .map_err(|e| format!("Failed to parse state file {}: {e}", path.display()))?;

    // Decrypt any v1 markers in resource outputs so callers see plaintext.
    // If no identity is available but markers exist, fail loudly here
    // rather than letting downstream code dereference an opaque marker.
    for (_name, res) in state.resources.iter_mut() {
        decrypt_outputs_in_place(&mut res.outputs, io.identities, path)?;
    }

    Ok(state)
}

pub fn write_state(path: &Path, state: &mut State, io: &StateIO) -> Result<(), String> {
    state.serial += 1;

    // Build a clone for serialization so the in-memory state stays
    // plaintext for downstream consumers (deploy-time resolution,
    // template rendering, etc.). Encrypting in the clone avoids
    // re-decrypting on subsequent in-process reads.
    let mut to_write = state.clone();
    for (name, res) in to_write.resources.iter_mut() {
        if let Some(schema) = io.schemas.schema(&res.resource_type) {
            encrypt_secret_outputs(&mut res.outputs, schema, name, io.recipients)?;
        }
    }
    let mut sorted_recipients = io.recipients_raw.to_vec();
    sorted_recipients.sort();
    to_write.encrypted_with = sorted_recipients.clone();

    let contents = serde_json::to_string_pretty(&to_write)
        .map_err(|e| format!("Failed to serialize state: {e}"))?;
    fs::write(path, contents)
        .map_err(|e| format!("Failed to write state file {}: {e}", path.display()))?;

    // Mirror encrypted_with onto the live state too, so subsequent
    // recipient-drift checks see the just-written set.
    state.encrypted_with = sorted_recipients;

    Ok(())
}

/// Count secret-output fields that currently hold a string value across
/// all resources in `state`. Used by `blue rekey` to report how many
/// values it re-encrypted. Counts the in-memory state — call this after
/// `read_state` (so values are plaintext) and before `write_state` (so
/// they haven't been replaced by markers yet).
pub fn count_secret_outputs(state: &State, schemas: &dyn SchemaResolver) -> usize {
    let mut count = 0;
    for res in state.resources.values() {
        let Some(schema) = schemas.schema(&res.resource_type) else {
            continue;
        };
        let Value::Object(map) = &res.outputs else {
            continue;
        };
        for output_def in &schema.outputs {
            if !output_def.secret {
                continue;
            }
            if let Some(Value::String(_)) = map.get(&output_def.path) {
                count += 1;
            }
        }
    }
    count
}

/// Copy `secret = true` fields from `old_outputs` into `new_outputs`
/// when they're missing from `new_outputs`. Load-bearing for write-only-
/// on-create secrets like UpCloud's `secret_access_key`: the API only
/// returns the value at create time, so refresh/update calls produce
/// outputs without it. Without this, refresh would clobber the saved
/// secret and the only copy is gone.
///
/// Top-level flat walk only — matches the schema's flat output shape.
pub fn preserve_secret_outputs(new_outputs: &mut Value, old_outputs: &Value, schema: &Schema) {
    let (Value::Object(new_map), Value::Object(old_map)) = (new_outputs, old_outputs) else {
        return;
    };
    for output_def in &schema.outputs {
        if !output_def.secret {
            continue;
        }
        if new_map.contains_key(&output_def.path) {
            continue;
        }
        if let Some(old_val) = old_map.get(&output_def.path) {
            new_map.insert(output_def.path.clone(), old_val.clone());
        }
    }
}

/// Walk a resource's outputs and decrypt every marker in place. Top-level
/// flat walk only — the schema layer doesn't currently produce nested
/// secret outputs, so a recursive walk would be overkill. Extend if that
/// ever changes.
fn decrypt_outputs_in_place(
    outputs: &mut Value,
    identities: &[Box<dyn age::Identity>],
    state_path: &Path,
) -> Result<(), String> {
    let Value::Object(map) = outputs else {
        return Ok(());
    };
    for (_key, val) in map.iter_mut() {
        let Value::String(s) = val else { continue };
        if !crypto::is_marker(s.as_str()) {
            continue;
        }
        if identities.is_empty() {
            return Err(format!(
                "state file '{}' contains encrypted values but no identity is configured \
                 (set BLUE_AGE_IDENTITY or BLUE_AGE_IDENTITY_KEY)",
                state_path.display()
            ));
        }
        *s = crypto::decrypt_value(s.as_str(), identities)?;
    }
    Ok(())
}

/// Walk a resource's outputs and encrypt every field flagged `secret = true`
/// in its schema. Idempotent: values that are already markers (e.g. a
/// post-rekey re-save with no decrypt step in between) are left unchanged.
/// If `recipients` is empty, this is a no-op — the plan-time check at
/// `plan.rs` is the gate that prevents secret-bearing resources from
/// reaching here without recipients configured.
fn encrypt_secret_outputs(
    outputs: &mut Value,
    schema: &Schema,
    resource_name: &str,
    recipients: &[age::x25519::Recipient],
) -> Result<(), String> {
    if recipients.is_empty() {
        return Ok(());
    }
    let Value::Object(map) = outputs else {
        return Ok(());
    };
    for output_def in &schema.outputs {
        if !output_def.secret {
            continue;
        }
        let Some(Value::String(s)) = map.get_mut(&output_def.path) else {
            continue;
        };
        if crypto::is_marker(s.as_str()) {
            continue;
        }
        let salt = format!("{resource_name}.{}", output_def.path);
        *s = crypto::encrypt_value(&salt, s.as_str(), recipients)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FieldType, OutputDef};
    use serde_json::json;
    use std::fs;
    use std::sync::Mutex;

    /// Some tests in this module also touch env vars indirectly via
    /// `crypto::load_identities`; serialize them with the same lock the
    /// crypto module uses, conceptually, by holding a local mutex for
    /// state-tests that read/write files in /tmp with shared paths.
    /// (Each test uses a uuid'd path, so this is mostly precautionary.)
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn schema_with_secret_output(path: &str) -> Schema {
        Schema {
            inputs: vec![],
            outputs: vec![OutputDef {
                path: path.to_string(),
                field_type: FieldType::String,
                secret: true,
            }],
            retry: None,
            timeout: None,
        }
    }

    /// SchemaResolver impl that returns a single schema for a single type.
    struct OneSchema(String, Schema);
    impl SchemaResolver for OneSchema {
        fn schema(&self, type_name: &str) -> Option<&Schema> {
            if type_name == self.0 {
                Some(&self.1)
            } else {
                None
            }
        }
    }

    fn tmp_path() -> std::path::PathBuf {
        std::path::PathBuf::from(format!(
            "/tmp/blue_test_state_{}.json",
            uuid::Uuid::new_v4()
        ))
    }

    #[test]
    fn new_state_has_lineage() {
        let state = State::new();
        assert!(!state.lineage.is_empty());
        assert_eq!(state.serial, 0);
        assert!(state.resources.is_empty());
        assert!(state.encrypted_with.is_empty());
    }

    #[test]
    fn read_nonexistent_returns_new() {
        let _g = TEST_LOCK.lock().unwrap();
        let state = read_state(
            Path::new("/tmp/blue_test_nonexistent_state.json"),
            &StateIO::plaintext(),
        )
        .unwrap();
        assert_eq!(state.serial, 0);
        assert!(state.resources.is_empty());
    }

    #[test]
    fn roundtrip_plaintext() {
        let _g = TEST_LOCK.lock().unwrap();
        let path = tmp_path();

        let mut state = State::new();
        state.resources.insert(
            "web-01".to_string(),
            ResourceState {
                resource_type: "upcloud.server".to_string(),
                inputs: json!({"hostname": "web-01", "zone": "uk-lon1"}),
                outputs: json!({"uuid": "abc-123", "state": "started"}),
                depends_on: vec!["resources.object-store".to_string()],
            },
        );

        write_state(&path, &mut state, &StateIO::plaintext()).unwrap();
        assert_eq!(state.serial, 1);

        let loaded = read_state(&path, &StateIO::plaintext()).unwrap();
        assert_eq!(loaded.lineage, state.lineage);
        assert_eq!(loaded.serial, 1);
        assert_eq!(loaded.resources.len(), 1);
        assert_eq!(loaded.resources["web-01"].outputs["uuid"], "abc-123");
        assert!(loaded.encrypted_with.is_empty());

        fs::remove_file(&path).ok();
    }

    #[test]
    fn serial_increments_on_each_write() {
        let _g = TEST_LOCK.lock().unwrap();
        let path = tmp_path();

        let mut state = State::new();
        write_state(&path, &mut state, &StateIO::plaintext()).unwrap();
        assert_eq!(state.serial, 1);
        write_state(&path, &mut state, &StateIO::plaintext()).unwrap();
        assert_eq!(state.serial, 2);

        let loaded = read_state(&path, &StateIO::plaintext()).unwrap();
        assert_eq!(loaded.serial, 2);

        fs::remove_file(&path).ok();
    }

    #[test]
    fn roundtrip_with_secret_field_encrypts_and_decrypts() {
        let _g = TEST_LOCK.lock().unwrap();
        let path = tmp_path();

        let id = age::x25519::Identity::generate();
        let recipient = id.to_public();
        let recipients = vec![recipient];
        let recipients_raw = vec![recipients[0].to_string()];
        let identities: Vec<Box<dyn age::Identity>> = vec![Box::new(id)];
        let resolver = OneSchema(
            "test.with_secret".to_string(),
            schema_with_secret_output("api_key"),
        );

        let io = StateIO {
            recipients: &recipients,
            identities: &identities,
            recipients_raw: &recipients_raw,
            schemas: &resolver,
        };

        let mut state = State::new();
        state.resources.insert(
            "myres".to_string(),
            ResourceState {
                resource_type: "test.with_secret".to_string(),
                inputs: json!({}),
                outputs: json!({"api_key": "AKIA-SUPER-SECRET", "public": "visible"}),
                depends_on: vec![],
            },
        );

        write_state(&path, &mut state, &io).unwrap();

        // On disk the secret should be wrapped in a marker; the public
        // field should still be plaintext.
        let raw = fs::read_to_string(&path).unwrap();
        assert!(
            !raw.contains("AKIA-SUPER-SECRET"),
            "secret leaked into state file: {raw}"
        );
        assert!(raw.contains("<blue:enc:v1:"), "no marker in state: {raw}");
        assert!(raw.contains("visible"), "non-secret field missing: {raw}");

        // Reading back through StateIO with the matching identity decrypts.
        let loaded = read_state(&path, &io).unwrap();
        assert_eq!(
            loaded.resources["myres"].outputs["api_key"],
            "AKIA-SUPER-SECRET"
        );
        assert_eq!(loaded.resources["myres"].outputs["public"], "visible");
        assert_eq!(loaded.encrypted_with, recipients_raw);

        fs::remove_file(&path).ok();
    }

    #[test]
    fn read_fails_when_markers_present_but_no_identity() {
        let _g = TEST_LOCK.lock().unwrap();
        let path = tmp_path();

        let id = age::x25519::Identity::generate();
        let recipients = vec![id.to_public()];
        let recipients_raw = vec![recipients[0].to_string()];
        let identities: Vec<Box<dyn age::Identity>> = vec![Box::new(id)];
        let resolver = OneSchema(
            "test.with_secret".to_string(),
            schema_with_secret_output("api_key"),
        );
        let write_io = StateIO {
            recipients: &recipients,
            identities: &identities,
            recipients_raw: &recipients_raw,
            schemas: &resolver,
        };

        let mut state = State::new();
        state.resources.insert(
            "r".to_string(),
            ResourceState {
                resource_type: "test.with_secret".to_string(),
                inputs: json!({}),
                outputs: json!({"api_key": "secret-1"}),
                depends_on: vec![],
            },
        );
        write_state(&path, &mut state, &write_io).unwrap();

        // Read with no identities — must fail loudly.
        let err = read_state(&path, &StateIO::plaintext()).unwrap_err();
        assert!(err.contains("encrypted values"), "got: {err}");
        assert!(err.contains("BLUE_AGE_IDENTITY"), "got: {err}");

        fs::remove_file(&path).ok();
    }

    #[test]
    fn write_skips_already_encrypted_value() {
        // If a value already looks like a marker (e.g. carried forward
        // by some other path), don't re-encrypt it on save. This keeps
        // intermediate ctx.save() calls in the same operation idempotent.
        let _g = TEST_LOCK.lock().unwrap();
        let path = tmp_path();

        let id = age::x25519::Identity::generate();
        let recipients = vec![id.to_public()];
        let recipients_raw = vec![recipients[0].to_string()];
        let identities: Vec<Box<dyn age::Identity>> = vec![Box::new(id)];
        let resolver = OneSchema(
            "test.with_secret".to_string(),
            schema_with_secret_output("api_key"),
        );
        let io = StateIO {
            recipients: &recipients,
            identities: &identities,
            recipients_raw: &recipients_raw,
            schemas: &resolver,
        };

        // Pre-encrypt a value, then put the marker into state directly.
        let marker = crypto::encrypt_value("myres.api_key", "secret-v1", &recipients).unwrap();
        let mut state = State::new();
        state.resources.insert(
            "myres".to_string(),
            ResourceState {
                resource_type: "test.with_secret".to_string(),
                inputs: json!({}),
                outputs: json!({"api_key": marker.clone()}),
                depends_on: vec![],
            },
        );

        write_state(&path, &mut state, &io).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(
            raw.contains(&marker),
            "expected marker to be preserved verbatim, got: {raw}"
        );

        fs::remove_file(&path).ok();
    }

    #[test]
    fn cleartext_secret_in_old_state_encrypts_on_next_write() {
        // Backward compat: a state file written before encryption was
        // wired up has a plaintext secret value. Reading it (with no
        // markers present) succeeds with no identity required. The next
        // write encrypts it.
        let _g = TEST_LOCK.lock().unwrap();
        let path = tmp_path();

        // Hand-write an "old" plaintext state file (no encrypted_with field).
        let raw = r#"{
            "lineage": "test-lineage",
            "serial": 5,
            "resources": {
                "myres": {
                    "type": "test.with_secret",
                    "inputs": {},
                    "outputs": {"api_key": "plain-text-secret"},
                    "depends_on": []
                }
            }
        }"#;
        fs::write(&path, raw).unwrap();

        // Read with no identity — succeeds because no markers exist.
        let mut state = read_state(&path, &StateIO::plaintext()).unwrap();
        assert_eq!(
            state.resources["myres"].outputs["api_key"],
            "plain-text-secret"
        );
        assert!(state.encrypted_with.is_empty());

        // Write with recipients + schema — value gets encrypted.
        let id = age::x25519::Identity::generate();
        let recipients = vec![id.to_public()];
        let recipients_raw = vec![recipients[0].to_string()];
        let identities: Vec<Box<dyn age::Identity>> = vec![Box::new(id)];
        let resolver = OneSchema(
            "test.with_secret".to_string(),
            schema_with_secret_output("api_key"),
        );
        let io = StateIO {
            recipients: &recipients,
            identities: &identities,
            recipients_raw: &recipients_raw,
            schemas: &resolver,
        };
        write_state(&path, &mut state, &io).unwrap();

        let after = fs::read_to_string(&path).unwrap();
        assert!(
            !after.contains("plain-text-secret"),
            "secret still plaintext: {after}"
        );
        assert!(
            after.contains("<blue:enc:v1:"),
            "no marker after upgrade: {after}"
        );

        fs::remove_file(&path).ok();
    }

    #[test]
    fn preserve_secret_outputs_carries_missing_secret_forward() {
        // The motivating case: provider's read() returned outputs without
        // the secret field; preserve copies it from old state.
        let schema = Schema {
            inputs: vec![],
            outputs: vec![
                OutputDef {
                    path: "username".to_string(),
                    field_type: FieldType::String,
                    secret: false,
                },
                OutputDef {
                    path: "secret_access_key".to_string(),
                    field_type: FieldType::String,
                    secret: true,
                },
            ],
            retry: None,
            timeout: None,
        };
        let old = json!({
            "username": "alice",
            "secret_access_key": "AKIA-OLD-SECRET",
        });
        let mut new = json!({
            "username": "alice",
        });

        preserve_secret_outputs(&mut new, &old, &schema);
        assert_eq!(new["secret_access_key"], "AKIA-OLD-SECRET");
        assert_eq!(new["username"], "alice");
    }

    #[test]
    fn preserve_secret_outputs_does_not_overwrite_present_secret() {
        // If the provider DID return the secret field (rotation scenario),
        // preserve must not clobber it with the old value.
        let schema = Schema {
            inputs: vec![],
            outputs: vec![OutputDef {
                path: "secret_access_key".to_string(),
                field_type: FieldType::String,
                secret: true,
            }],
            retry: None,
            timeout: None,
        };
        let old = json!({"secret_access_key": "OLD"});
        let mut new = json!({"secret_access_key": "NEW"});

        preserve_secret_outputs(&mut new, &old, &schema);
        assert_eq!(new["secret_access_key"], "NEW");
    }

    #[test]
    fn preserve_secret_outputs_ignores_non_secret_fields() {
        // Non-secret fields are NOT preserved — refresh should reflect
        // their actual current state from the provider.
        let schema = Schema {
            inputs: vec![],
            outputs: vec![OutputDef {
                path: "status".to_string(),
                field_type: FieldType::String,
                secret: false,
            }],
            retry: None,
            timeout: None,
        };
        let old = json!({"status": "Active"});
        let mut new = json!({});

        preserve_secret_outputs(&mut new, &old, &schema);
        assert!(new.as_object().unwrap().is_empty());
    }

    #[test]
    fn rekey_adding_recipient_makes_state_readable_to_new_identity() {
        // Encrypt with R1; rekey to {R1, R2}; confirm I2 can now decrypt.
        let _g = TEST_LOCK.lock().unwrap();
        let path = tmp_path();

        let id_a = age::x25519::Identity::generate();
        let id_b = age::x25519::Identity::generate();
        let rec_a = id_a.to_public();
        let rec_b = id_b.to_public();
        let resolver = OneSchema(
            "test.with_secret".to_string(),
            schema_with_secret_output("api_key"),
        );

        // Initial state, encrypted under R1 only.
        let recipients_v1 = vec![rec_a.clone()];
        let recipients_raw_v1 = vec![rec_a.to_string()];
        let identities_a: Vec<Box<dyn age::Identity>> = vec![Box::new(id_a.clone())];
        let io_v1 = StateIO {
            recipients: &recipients_v1,
            identities: &identities_a,
            recipients_raw: &recipients_raw_v1,
            schemas: &resolver,
        };
        let mut state = State::new();
        state.resources.insert(
            "myres".to_string(),
            ResourceState {
                resource_type: "test.with_secret".to_string(),
                inputs: json!({}),
                outputs: json!({"api_key": "ROTATEME"}),
                depends_on: vec![],
            },
        );
        write_state(&path, &mut state, &io_v1).unwrap();

        // Rekey: read with I1 (decrypts R1 ciphertext), write with R1+R2.
        let read_io = StateIO {
            recipients: &[],
            identities: &identities_a,
            recipients_raw: &[],
            schemas: &resolver,
        };
        let mut state = read_state(&path, &read_io).unwrap();
        assert_eq!(state.resources["myres"].outputs["api_key"], "ROTATEME");

        let recipients_v2 = vec![rec_a.clone(), rec_b.clone()];
        let recipients_raw_v2 = vec![rec_a.to_string(), rec_b.to_string()];
        let io_v2 = StateIO {
            recipients: &recipients_v2,
            identities: &identities_a,
            recipients_raw: &recipients_raw_v2,
            schemas: &resolver,
        };
        write_state(&path, &mut state, &io_v2).unwrap();

        // Now I2 (which never had access before) can decrypt.
        let identities_b: Vec<Box<dyn age::Identity>> = vec![Box::new(id_b)];
        let io_b = StateIO {
            recipients: &[],
            identities: &identities_b,
            recipients_raw: &[],
            schemas: &resolver,
        };
        let loaded = read_state(&path, &io_b).unwrap();
        assert_eq!(loaded.resources["myres"].outputs["api_key"], "ROTATEME");

        fs::remove_file(&path).ok();
    }

    #[test]
    fn rekey_removing_recipient_locks_old_identity_out() {
        let _g = TEST_LOCK.lock().unwrap();
        let path = tmp_path();

        let id_a = age::x25519::Identity::generate();
        let id_b = age::x25519::Identity::generate();
        let rec_a = id_a.to_public();
        let rec_b = id_b.to_public();
        let resolver = OneSchema(
            "test.with_secret".to_string(),
            schema_with_secret_output("api_key"),
        );

        // Start with both recipients.
        let recipients_v1 = vec![rec_a.clone(), rec_b.clone()];
        let recipients_raw_v1 = vec![rec_a.to_string(), rec_b.to_string()];
        let identities_both: Vec<Box<dyn age::Identity>> =
            vec![Box::new(id_a.clone()), Box::new(id_b.clone())];
        let io_v1 = StateIO {
            recipients: &recipients_v1,
            identities: &identities_both,
            recipients_raw: &recipients_raw_v1,
            schemas: &resolver,
        };
        let mut state = State::new();
        state.resources.insert(
            "myres".to_string(),
            ResourceState {
                resource_type: "test.with_secret".to_string(),
                inputs: json!({}),
                outputs: json!({"api_key": "REVOKE"}),
                depends_on: vec![],
            },
        );
        write_state(&path, &mut state, &io_v1).unwrap();

        // Rekey to drop B: read with both, write with only A.
        let read_io = StateIO {
            recipients: &[],
            identities: &identities_both,
            recipients_raw: &[],
            schemas: &resolver,
        };
        let mut state = read_state(&path, &read_io).unwrap();

        let recipients_v2 = vec![rec_a.clone()];
        let recipients_raw_v2 = vec![rec_a.to_string()];
        let identities_a: Vec<Box<dyn age::Identity>> = vec![Box::new(id_a)];
        let io_v2 = StateIO {
            recipients: &recipients_v2,
            identities: &identities_a,
            recipients_raw: &recipients_raw_v2,
            schemas: &resolver,
        };
        write_state(&path, &mut state, &io_v2).unwrap();

        // I2 alone can no longer decrypt the rewritten ciphertext.
        let identities_b_only: Vec<Box<dyn age::Identity>> = vec![Box::new(id_b)];
        let io_b = StateIO {
            recipients: &[],
            identities: &identities_b_only,
            recipients_raw: &[],
            schemas: &resolver,
        };
        let err = read_state(&path, &io_b).unwrap_err();
        assert!(
            err.contains("decrypt") || err.contains("identity"),
            "expected decrypt failure, got: {err}"
        );

        fs::remove_file(&path).ok();
    }

    #[test]
    fn rekey_with_no_secrets_in_state_just_updates_recipient_metadata() {
        // No resources have secret outputs → write produces no markers,
        // but encrypted_with gets the new sorted recipient list anyway.
        let _g = TEST_LOCK.lock().unwrap();
        let path = tmp_path();

        let id_a = age::x25519::Identity::generate();
        let rec_a = id_a.to_public();
        let recipients = vec![rec_a.clone()];
        let recipients_raw = vec![rec_a.to_string()];
        let identities: Vec<Box<dyn age::Identity>> = vec![Box::new(id_a)];
        let io = StateIO {
            recipients: &recipients,
            identities: &identities,
            recipients_raw: &recipients_raw,
            schemas: &NoSchemas,
        };

        let mut state = State::new();
        state.resources.insert(
            "r".to_string(),
            ResourceState {
                resource_type: "test.no_secrets".to_string(),
                inputs: json!({}),
                outputs: json!({"public": "x"}),
                depends_on: vec![],
            },
        );
        write_state(&path, &mut state, &io).unwrap();

        let count = count_secret_outputs(&state, &NoSchemas);
        assert_eq!(count, 0);
        assert_eq!(state.encrypted_with, recipients_raw);

        fs::remove_file(&path).ok();
    }

    #[test]
    fn count_secret_outputs_counts_only_present_secret_string_fields() {
        let resolver = OneSchema(
            "test.with_secret".to_string(),
            schema_with_secret_output("api_key"),
        );
        let mut state = State::new();
        state.resources.insert(
            "has_secret".to_string(),
            ResourceState {
                resource_type: "test.with_secret".to_string(),
                inputs: json!({}),
                outputs: json!({"api_key": "AKIA1234"}),
                depends_on: vec![],
            },
        );
        // A second resource of unknown type — schema lookup returns None,
        // contributes 0 to the count.
        state.resources.insert(
            "unknown".to_string(),
            ResourceState {
                resource_type: "test.unknown".to_string(),
                inputs: json!({}),
                outputs: json!({"api_key": "fake"}),
                depends_on: vec![],
            },
        );
        // A third resource with the schema-known type but no secret value
        // present in outputs — also contributes 0.
        state.resources.insert(
            "missing_secret".to_string(),
            ResourceState {
                resource_type: "test.with_secret".to_string(),
                inputs: json!({}),
                outputs: json!({}),
                depends_on: vec![],
            },
        );

        assert_eq!(count_secret_outputs(&state, &resolver), 1);
    }

    #[test]
    fn encrypted_with_records_sorted_recipients() {
        let _g = TEST_LOCK.lock().unwrap();
        let path = tmp_path();

        // Two recipients in arbitrary order; encrypted_with must come back
        // sorted regardless.
        let id_a = age::x25519::Identity::generate();
        let id_b = age::x25519::Identity::generate();
        let rec_a = id_a.to_public().to_string();
        let rec_b = id_b.to_public().to_string();
        let recipients_raw = vec![rec_b.clone(), rec_a.clone()];

        let recipients = crypto::parse_recipients(&recipients_raw).unwrap();
        let identities: Vec<Box<dyn age::Identity>> = vec![Box::new(id_a)];
        let resolver = NoSchemas;
        let io = StateIO {
            recipients: &recipients,
            identities: &identities,
            recipients_raw: &recipients_raw,
            schemas: &resolver,
        };

        let mut state = State::new();
        write_state(&path, &mut state, &io).unwrap();

        let mut expected = recipients_raw.clone();
        expected.sort();
        assert_eq!(state.encrypted_with, expected);

        let loaded = read_state(&path, &io).unwrap();
        assert_eq!(loaded.encrypted_with, expected);

        fs::remove_file(&path).ok();
    }
}
