// Hooks module
// Main entry point for hook functionality

pub mod runtime;

use crate::state::State;
use serde_json::Value;
use std::time::Duration;

pub struct HookContext {
    pub state: State,
    pub resource_type: String,
    pub resource_name: String,
}

impl HookContext {
    pub fn new(state: State, resource_type: String, resource_name: String) -> Self {
        HookContext {
            state,
            resource_type,
            resource_name,
        }
    }
    
    pub fn filter_sensitive_data(&self) -> serde_json::Value {
        // Filter out sensitive data from state before passing to hooks
        // This is a placeholder - actual implementation will depend on schema
        serde_json::json!({
            "resources": self.state.resources,
        })
    }
}

/// Execute a hook with full validation and error handling
pub async fn execute_hook_with_validation(
    hook: &crate::config::Hook,
    context: HookContext,
    timeout: Duration,
) -> Result<Value, String> {
    let runtime = runtime::HookRuntime::new();
    
    // Create context object for the hook
    let context_json = context.filter_sensitive_data().to_string();
    
    // Prepare the script with context injection
    let full_script = format!(
        "const context = {};\n{}",
        context_json,
        hook.script
    );
    
    // Execute with timeout
    let output = runtime.execute_script(&full_script, timeout).await?;
    
    // Parse output as JSON
    let output_value: Value = match serde_json::from_str(&output) {
        Ok(v) => v,
        Err(e) => return Err(format!("Hook output is not valid JSON: {}", e)),
    };
    
    // Validate against schema if outputs are declared
    if !hook.outputs.is_empty() {
        validate_hook_outputs(&output_value, &hook.outputs)?;
    }
    
    Ok(output_value)
}

/// Execute a single hook without validation (for simple cases)
pub async fn execute_hook(
    hook_type: &str,
    script: &str,
    context: HookContext,
    timeout: Duration,
) -> Result<String, String> {
    let runtime = runtime::HookRuntime::new();
    
    // Create context object for the hook
    let context_json = context.filter_sensitive_data().to_string();
    
    // Prepare the script with context injection
    let full_script = format!(
        "const context = {};\n{}",
        context_json,
        script
    );
    
    // Execute with timeout
    let output = runtime.execute_script(&full_script, timeout).await?;
    
    Ok(output)
}

/// Validate hook outputs against declared schema
pub fn validate_hook_outputs(
    outputs: &Value,
    expected_outputs: &[crate::config::HookOutput],
) -> Result<(), String> {
    // Parse the output string as JSON
    let output_value: Value = match serde_json::from_str(&outputs.to_string()) {
        Ok(v) => v,
        Err(e) => return Err(format!("Invalid JSON output: {}", e)),
    };
    
    // Check each expected output
    for expected in expected_outputs {
        let actual = output_value.get(&expected.name)
            .ok_or_else(|| format!("Missing required output: {}", expected.name))?;
        
        // Validate type matches
        match expected.r#type.as_str() {
            "string" => {
                if !actual.is_string() {
                    return Err(format!(
                        "Output '{}' expected string, got {}",
                        expected.name,
                        get_json_type(actual)
                    ));
                }
            },
            "integer" => {
                if !actual.is_number() || actual.as_f64().map_or(false, |n| n.fract() != 0.0) {
                    return Err(format!(
                        "Output '{}' expected integer, got {}",
                        expected.name,
                        get_json_type(actual)
                    ));
                }
            },
            "float" => {
                if !actual.is_number() {
                    return Err(format!(
                        "Output '{}' expected float, got {}",
                        expected.name,
                        get_json_type(actual)
                    ));
                }
            },
            "boolean" => {
                if !actual.is_boolean() {
                    return Err(format!(
                        "Output '{}' expected boolean, got {}",
                        expected.name,
                        get_json_type(actual)
                    ));
                }
            },
            "array" => {
                if !actual.is_array() {
                    return Err(format!(
                        "Output '{}' expected array, got {}",
                        expected.name,
                        get_json_type(actual)
                    ));
                }
            },
            _ => return Err(format!("Unknown output type: {}", expected.r#type)),
        }
    }
    
    Ok(())
}

fn get_json_type(value: &Value) -> &'static str {
    if value.is_string() {
        "string"
    } else if value.is_number() {
        "number"
    } else if value.is_boolean() {
        "boolean"
    } else if value.is_array() {
        "array"
    } else if value.is_object() {
        "object"
    } else if value.is_null() {
        "null"
    } else {
        "unknown"
    }
}