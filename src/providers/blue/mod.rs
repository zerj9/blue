mod runtime;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use serde_json::{Value, json};

use crate::provider::{
    DataSourceType, OperationCtx, ProviderInstance, Providers, ResourceType,
};
use crate::types::{Action, Diff, OperationResult, Schema};

use runtime::ScriptRuntime;

// --- Script Resource ---

pub struct ScriptResource {
    schema: Schema,
    runtime: ScriptRuntime,
    config_dir: Option<PathBuf>,
}

impl ScriptResource {
    pub fn new(config_dir: Option<PathBuf>) -> Self {
        let schema = crate::schema::parse_schema(include_str!("schemas/blue_script_resource.toml"))
            .expect("built-in schema must be valid");
        ScriptResource {
            schema,
            runtime: ScriptRuntime::new(),
            config_dir,
        }
    }

    fn resolve_script_path(&self, script: &str) -> Result<PathBuf, String> {
        let config_dir = self.config_dir.as_ref()
            .ok_or_else(|| "No config directory available for script resolution".to_string())?;

        let script_path = config_dir.join(script);
        let canonical_dir = config_dir.canonicalize()
            .map_err(|e| format!("Failed to canonicalize config directory: {e}"))?;
        let canonical_script = script_path.canonicalize()
            .map_err(|e| format!("Failed to resolve script path '{}': {e}", script_path.display()))?;

        if !canonical_script.starts_with(&canonical_dir) {
            return Err(format!(
                "Script '{}' escapes config directory '{}'",
                script, config_dir.display()
            ));
        }

        Ok(canonical_script)
    }

    fn timeout_secs(&self) -> u64 {
        self.schema.timeout.as_ref().map_or(60, |t| t.seconds)
    }

    fn hash_script_file(&self, script: &str) -> Result<String, String> {
        let script_path = self.resolve_script_path(script)?;
        let contents = std::fs::read_to_string(&script_path)
            .map_err(|e| format!("Failed to read script '{}': {e}", script_path.display()))?;
        let mut hasher = DefaultHasher::new();
        contents.hash(&mut hasher);
        Ok(format!("{:x}", hasher.finish()))
    }
}

impl ResourceType for ScriptResource {
    fn schema(&self) -> &Schema {
        &self.schema
    }

    fn create(&self, _ctx: &dyn OperationCtx, inputs: Value) -> Result<OperationResult, String> {
        let script = inputs.get("script")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing 'script' input".to_string())?;

        let script_path = self.resolve_script_path(script)?;

        // Build context: inputs minus script and triggers_replace
        let mut script_inputs = inputs.as_object().cloned().unwrap_or_default();
        script_inputs.remove("script");
        script_inputs.remove("triggers_replace");

        let context = json!({
            "operation": "create",
            "inputs": script_inputs,
            "outputs": {}
        });

        let mut result = self.runtime.run_script(
            &script_path,
            context,
            self.timeout_secs(),
            script,
        )?;

        // Store script content hash so customize_diff can detect file changes
        if let Some(obj) = result.as_object_mut() {
            let hash = self.hash_script_file(script)?;
            obj.insert("__script_hash__".to_string(), Value::String(hash));
        }

        Ok(OperationResult::Success { outputs: result })
    }

    fn read(&self, outputs: &Value) -> Result<OperationResult, String> {
        // Script resources have no external state — return stored outputs
        Ok(OperationResult::Success {
            outputs: outputs.clone(),
        })
    }

    fn update(
        &self,
        _ctx: &dyn OperationCtx,
        old_outputs: &Value,
        _new_inputs: Value,
    ) -> Result<OperationResult, String> {
        // No-op — inputs updated in state, script not re-run
        Ok(OperationResult::Success {
            outputs: old_outputs.clone(),
        })
    }

    fn delete(&self, _ctx: &dyn OperationCtx, _outputs: &Value) -> Result<OperationResult, String> {
        // No-op — blue.script has no external state to clean up
        Ok(OperationResult::Success {
            outputs: json!({}),
        })
    }

    fn customize_diff(&self, diff: &mut Diff, inputs: &Value, outputs: &Value) -> Result<(), String> {
        // If already creating or deleting, nothing to customize
        if diff.action == Action::Create || diff.action == Action::Delete {
            return Ok(());
        }

        // Check if the script file contents have changed since last deploy
        let Some(script) = inputs.get("script").and_then(|v| v.as_str()) else {
            return Ok(());
        };
        let stored_hash = outputs.get("__script_hash__").and_then(|v| v.as_str()).unwrap_or("");
        let current_hash = self.hash_script_file(script).unwrap_or_default();

        if !stored_hash.is_empty() && current_hash != stored_hash {
            diff.action = Action::Replace;
        } else if stored_hash.is_empty() && !current_hash.is_empty() {
            // No hash in state (pre-existing resource) — trigger replace to store hash
            diff.action = Action::Replace;
        }

        Ok(())
    }
}

// --- Script Data Source ---

pub struct ScriptDataSource {
    schema: Schema,
    runtime: ScriptRuntime,
    config_dir: Option<PathBuf>,
}

impl ScriptDataSource {
    pub fn new(config_dir: Option<PathBuf>) -> Self {
        let schema =
            crate::schema::parse_schema(include_str!("schemas/blue_script_data_source.toml"))
                .expect("built-in schema must be valid");
        ScriptDataSource {
            schema,
            runtime: ScriptRuntime::new(),
            config_dir,
        }
    }

    fn resolve_script_path(&self, script: &str) -> Result<PathBuf, String> {
        let config_dir = self.config_dir.as_ref()
            .ok_or_else(|| "No config directory available for script resolution".to_string())?;

        let script_path = config_dir.join(script);
        let canonical_dir = config_dir.canonicalize()
            .map_err(|e| format!("Failed to canonicalize config directory: {e}"))?;
        let canonical_script = script_path.canonicalize()
            .map_err(|e| format!("Failed to resolve script path '{}': {e}", script_path.display()))?;

        if !canonical_script.starts_with(&canonical_dir) {
            return Err(format!(
                "Script '{}' escapes config directory '{}'",
                script, config_dir.display()
            ));
        }

        Ok(canonical_script)
    }

    fn timeout_secs(&self) -> u64 {
        self.schema.timeout.as_ref().map_or(60, |t| t.seconds)
    }
}

impl DataSourceType for ScriptDataSource {
    fn schema(&self) -> &Schema {
        &self.schema
    }

    fn read(&self, inputs: Value) -> Result<Value, String> {
        let script = inputs.get("script")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing 'script' input".to_string())?;

        let script_path = self.resolve_script_path(script)?;

        // Build context: inputs minus script
        let mut script_inputs = inputs.as_object().cloned().unwrap_or_default();
        script_inputs.remove("script");

        let context = json!({
            "inputs": script_inputs
        });

        self.runtime.run_script(
            &script_path,
            context,
            self.timeout_secs(),
            script,
        )
    }
}

// --- Blue Provider Instance ---

pub struct BlueProvider {
    script_resource: ScriptResource,
    script_data_source: ScriptDataSource,
}

impl ProviderInstance for BlueProvider {
    fn resource_type(&self, name: &str) -> Option<&dyn ResourceType> {
        match name {
            "script" => Some(&self.script_resource),
            _ => None,
        }
    }

    fn data_source_type(&self, name: &str) -> Option<&dyn DataSourceType> {
        match name {
            "script" => Some(&self.script_data_source),
            _ => None,
        }
    }
}

pub fn register(providers: &mut Providers, config_dir: Option<PathBuf>) {
    let instance = BlueProvider {
        script_resource: ScriptResource::new(config_dir.clone()),
        script_data_source: ScriptDataSource::new(config_dir),
    };
    providers.register("blue", Box::new(instance));
}
