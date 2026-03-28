// Hooks module
// Main entry point for hook functionality

pub mod runtime;

use crate::state::State;
use serde_json::Value;
use std::time::Duration;

// TODO: HookContext clones the full State for every hook invocation.
// Accept a reference or pre-serialized JSON instead.
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
    
    // Read the script file
    let script_code = std::fs::read_to_string(&hook.script)
        .map_err(|e| format!("Failed to read hook script '{}': {}", hook.script, e))?;

    // Prepare the script with context injection
    let full_script = format!(
        "const context = {};\n{}",
        context_json,
        script_code
    );
    
    // Execute with timeout
    let log_prefix = format!("{}.{}:{}", context.resource_type, context.resource_name, hook.event);
    let output_value = runtime.execute_script(&full_script, &log_prefix, timeout).await?;

    // Validate against schema if outputs are declared
    if !hook.outputs.is_empty() {
        validate_hook_outputs(&output_value, &hook.outputs)?;
    }

    Ok(output_value)
}

/// Execute all hooks for a data source and insert outputs into the registry.
pub fn execute_data_hooks(
    hooks: &[crate::config::Hook],
    name: &str,
    output_registry: &mut crate::reference::OutputRegistry,
) -> Result<(), String> {
    let rt = tokio::runtime::Runtime::new().unwrap();
    for hook in hooks {
        let context = HookContext::new(
            State { version: 1, serial: 0, data: std::collections::HashMap::new(), resources: std::collections::HashMap::new() },
            "data".to_string(),
            name.to_string(),
        );
        let output = rt.block_on(execute_hook_with_validation(hook, context, Duration::from_secs(10)))
            .map_err(|e| format!("data.{name}: {e}"))?;
        insert_hook_outputs(output_registry, "data", name, &output);
    }
    Ok(())
}

/// Execute plan-safe hooks for a resource and insert outputs into the registry.
pub fn execute_safe_resource_hooks(
    hooks: &[crate::config::Hook],
    name: &str,
    output_registry: &mut crate::reference::OutputRegistry,
) -> Result<(), String> {
    let rt = tokio::runtime::Runtime::new().unwrap();
    for hook in hooks {
        match hook.event.as_str() {
            "before_create" | "before_update" => {
                let context = HookContext::new(
                    State { version: 1, serial: 0, data: std::collections::HashMap::new(), resources: std::collections::HashMap::new() },
                    "resource".to_string(),
                    name.to_string(),
                );
                let output = rt.block_on(execute_hook_with_validation(hook, context, Duration::from_secs(10)))
                    .map_err(|e| format!("resources.{name}: {e}"))?;
                insert_hook_outputs(output_registry, "resources", name, &output);
            }
            _ => {}
        }
    }
    Ok(())
}

fn insert_hook_outputs(
    registry: &mut crate::reference::OutputRegistry,
    source: &str,
    name: &str,
    output: &Value,
) {
    if let Some(obj) = output.as_object() {
        for (k, v) in obj {
            registry.insert(source, name, &format!("hooks.outputs.{k}"), v.clone());
        }
    }
}

/// Validate hook outputs against declared schema
pub fn validate_hook_outputs(
    outputs: &Value,
    expected_outputs: &[crate::config::HookOutput],
) -> Result<(), String> {
    for expected in expected_outputs {
        let actual = outputs.get(&expected.name)
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