use std::time::Duration;

use deno_core::{JsRuntime, RuntimeOptions, v8};

const CONSOLE_JS: &str = include_str!("console.js");

pub struct HookRuntime;

impl HookRuntime {
    pub fn new() -> Self {
        HookRuntime
    }

    pub async fn execute_script(&self, script: &str, log_prefix: &str, _timeout: Duration) -> Result<serde_json::Value, String> {
        let mut runtime = JsRuntime::new(RuntimeOptions::default());

        // Inject log prefix and console, wrap user script in IIFE so return works
        let full_script = format!("const __log_prefix__ = '{log_prefix}';\n{CONSOLE_JS}\n(function() {{\n{script}\n}})()");

        let global = runtime
            .execute_script("<hook>", full_script)
            .map_err(|e| format!("Hook execution failed: {e}"))?;

        // Deserialize the script's return value directly
        deno_core::scope!(scope, runtime);
        let local = v8::Local::new(scope, global);
        let value: serde_json::Value = deno_core::serde_v8::from_v8(scope, local)
            .map_err(|e| format!("Failed to deserialize hook output: {e}"))?;

        // If the script returned null/undefined, default to empty object
        if value.is_null() {
            Ok(serde_json::json!({}))
        } else {
            Ok(value)
        }
    }
}
