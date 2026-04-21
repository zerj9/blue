use std::path::Path;
use std::time::Duration;

use deno_core::{JsRuntime, RuntimeOptions, v8};
use serde_json::Value;

const CONSOLE_JS: &str = include_str!("console.js");
const DEFAULT_TIMEOUT_SECS: u64 = 60;

pub struct ScriptRuntime;

impl ScriptRuntime {
    pub fn new() -> Self {
        ScriptRuntime
    }

    pub fn run_script(
        &self,
        script_path: &Path,
        context: Value,
        timeout_secs: u64,
        log_prefix: &str,
    ) -> Result<Value, String> {
        // Validate script exists
        if !script_path.exists() {
            return Err(format!(
                "Script file not found: {}",
                script_path.display()
            ));
        }

        let script = std::fs::read_to_string(script_path)
            .map_err(|e| format!("Failed to read script '{}': {e}", script_path.display()))?;

        let context_json = context.to_string();

        let timeout = if timeout_secs == 0 {
            Duration::from_secs(DEFAULT_TIMEOUT_SECS)
        } else {
            Duration::from_secs(timeout_secs)
        };

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(|e| format!("Failed to create tokio runtime: {e}"))?;

        rt.block_on(async {
            tokio::time::timeout(timeout, async {
                execute_script(&script, &context_json, log_prefix)
            })
            .await
            .map_err(|_| format!("Script timed out after {}s: {}", timeout_secs, script_path.display()))?
        })
    }
}

fn execute_script(
    script: &str,
    context_json: &str,
    log_prefix: &str,
) -> Result<Value, String> {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());

    let full_script = format!(
        "const __log_prefix__ = '{log_prefix}';\n{CONSOLE_JS}\nconst context = {context_json};\n(function() {{\n{script}\n}})()"
    );

    let global = runtime
        .execute_script("<script>", full_script)
        .map_err(|e| format!("Script execution failed: {e}"))?;

    deno_core::scope!(scope, runtime);
    let local = v8::Local::new(scope, global);
    let value: Value = deno_core::serde_v8::from_v8(scope, local)
        .map_err(|e| format!("Failed to deserialize script output: {e}"))?;

    if value.is_null() {
        Ok(serde_json::json!({}))
    } else {
        Ok(value)
    }
}
