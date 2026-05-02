use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::Value;

use crate::plan::Plan;
use crate::provider::{OperationCtx, Providers};
use crate::state::{ResourceState, State, write_state};
use crate::types::{Action, OperationResult};

struct DeployCtx {
    state: Arc<Mutex<State>>,
    state_path: Arc<Path>,
    resource_name: String,
    resource_type: String,
    depends_on: Vec<String>,
}

impl OperationCtx for DeployCtx {
    fn save(&self, outputs: &Value) {
        let mut state = self.state.lock().unwrap();
        if let Some(res) = state.resources.get_mut(&self.resource_name) {
            res.outputs = outputs.clone();
        } else {
            state.resources.insert(
                self.resource_name.clone(),
                ResourceState {
                    resource_type: self.resource_type.clone(),
                    inputs: Value::Object(serde_json::Map::new()),
                    outputs: outputs.clone(),
                    depends_on: self.depends_on.clone(),
                },
            );
        }
        if let Err(e) = write_state(&self.state_path, &mut state) {
            eprintln!("  Warning: failed to save intermediate state: {e}");
        }
    }
}

pub fn execute_deploy(
    plan: &Plan,
    state: &mut State,
    state_path: &Path,
    providers: &Providers,
) -> Result<(), String> {
    // Staleness check
    if state.lineage != plan.lineage || state.serial != plan.serial {
        return Err(format!(
            "State has changed since plan was created (expected serial {}, got {}). Re-run plan.",
            plan.serial, state.serial
        ));
    }

    let state_arc = Arc::new(Mutex::new(state.clone()));
    let path_arc: Arc<Path> = Arc::from(state_path);

    for step in &plan.steps {
        let res_type = providers
            .resource_type(&step.resource_type)
            .ok_or_else(|| format!("Unknown resource type: {}", step.resource_type))?;

        let ctx = DeployCtx {
            state: Arc::clone(&state_arc),
            state_path: Arc::clone(&path_arc),
            resource_name: step.name.clone(),
            resource_type: step.resource_type.clone(),
            depends_on: step.depends_on.clone(),
        };

        let retry = res_type.schema().retry.as_ref();
        let max_attempts = retry.map_or(1, |r| r.max_attempts);
        let interval = retry.map_or(0, |r| r.interval_seconds);

        let result = match &step.action {
            Action::Create => execute_with_retry(max_attempts, interval, &ctx, || {
                res_type.create(&ctx, step.resolved_inputs.clone().unwrap_or_default())
            }),
            Action::Update => execute_with_retry(max_attempts, interval, &ctx, || {
                let old_outputs = get_outputs(&state_arc, &step.name);
                res_type.update(
                    &ctx,
                    &old_outputs,
                    step.resolved_inputs.clone().unwrap_or_default(),
                )
            }),
            Action::Delete => execute_with_retry(max_attempts, interval, &ctx, || {
                let outputs = get_outputs(&state_arc, &step.name);
                res_type.delete(&ctx, &outputs)
            }),
            Action::Replace => {
                let delete_result = execute_with_retry(max_attempts, interval, &ctx, || {
                    let outputs = get_outputs(&state_arc, &step.name);
                    res_type.delete(&ctx, &outputs)
                });
                match delete_result {
                    Ok(OperationResult::Success { .. }) | Ok(OperationResult::NotFound) => {
                        let mut locked = state_arc.lock().unwrap();
                        locked.resources.remove(&step.name);
                        write_state(&path_arc, &mut locked)?;
                    }
                    Ok(OperationResult::Failed { error, .. }) => {
                        sync_state(state, &state_arc);
                        return Err(format!(
                            "Failed to delete '{}' for replace: {error}",
                            step.name
                        ));
                    }
                    Err(e) => {
                        sync_state(state, &state_arc);
                        return Err(format!("Error deleting '{}' for replace: {e}", step.name));
                    }
                }
                execute_with_retry(max_attempts, interval, &ctx, || {
                    res_type.create(&ctx, step.resolved_inputs.clone().unwrap_or_default())
                })
            }
            Action::Unchanged => continue,
        };

        match (&step.action, result) {
            (Action::Delete, Ok(OperationResult::Success { .. } | OperationResult::NotFound)) => {
                let mut locked = state_arc.lock().unwrap();
                locked.resources.remove(&step.name);
                write_state(&path_arc, &mut locked)?;
            }
            (_, Ok(OperationResult::Success { outputs })) => {
                let mut locked = state_arc.lock().unwrap();
                locked.resources.insert(
                    step.name.clone(),
                    ResourceState {
                        resource_type: step.resource_type.clone(),
                        inputs: step.resolved_inputs.clone().unwrap_or_default(),
                        outputs,
                        depends_on: step.depends_on.clone(),
                    },
                );
                write_state(&path_arc, &mut locked)?;
            }
            (_, Ok(OperationResult::NotFound)) => {
                let mut locked = state_arc.lock().unwrap();
                locked.resources.remove(&step.name);
                write_state(&path_arc, &mut locked)?;
            }
            (_, Ok(OperationResult::Failed { error, outputs })) => {
                if let Some(outputs) = outputs {
                    let mut locked = state_arc.lock().unwrap();
                    locked.resources.insert(
                        step.name.clone(),
                        ResourceState {
                            resource_type: step.resource_type.clone(),
                            inputs: step.resolved_inputs.clone().unwrap_or_default(),
                            outputs,
                            depends_on: step.depends_on.clone(),
                        },
                    );
                    write_state(&path_arc, &mut locked)?;
                }
                sync_state(state, &state_arc);
                return Err(format!("Failed to deploy '{}': {error}", step.name));
            }
            (_, Err(e)) => {
                sync_state(state, &state_arc);
                return Err(format!("Error deploying '{}': {e}", step.name));
            }
        }
    }

    sync_state(state, &state_arc);
    Ok(())
}

fn execute_with_retry<F>(
    max_attempts: u32,
    interval: u64,
    ctx: &DeployCtx,
    f: F,
) -> Result<OperationResult, String>
where
    F: Fn() -> Result<OperationResult, String>,
{
    for attempt in 1..=max_attempts {
        match f() {
            Ok(OperationResult::Failed { error, outputs }) if attempt < max_attempts => {
                if let Some(ref o) = outputs {
                    ctx.save(o);
                }
                eprintln!(
                    "  Attempt {attempt}/{max_attempts} failed: {error}, retrying in {interval}s..."
                );
                thread::sleep(Duration::from_secs(interval));
            }
            Err(e) if attempt < max_attempts => {
                eprintln!(
                    "  Attempt {attempt}/{max_attempts} error: {e}, retrying in {interval}s..."
                );
                thread::sleep(Duration::from_secs(interval));
            }
            result => return result,
        }
    }
    unreachable!()
}

fn get_outputs(state_arc: &Arc<Mutex<State>>, name: &str) -> Value {
    state_arc
        .lock()
        .unwrap()
        .resources
        .get(name)
        .map(|r| r.outputs.clone())
        .unwrap_or_default()
}

fn sync_state(state: &mut State, state_arc: &Arc<Mutex<State>>) {
    let locked = state_arc.lock().unwrap();
    *state = locked.clone();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse_resource_config;
    use crate::plan::create_plan;
    use crate::providers::blue;
    use serde_json::json;
    use std::collections::HashMap;
    use std::fs;

    fn setup() -> (Providers, String, std::path::PathBuf) {
        let tmp_dir = std::path::PathBuf::from(format!("/tmp/blue_test_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&tmp_dir).unwrap();

        // Write a dummy script that returns the inputs as outputs
        fs::write(tmp_dir.join("test.js"), "return context.inputs;").unwrap();
        fs::write(tmp_dir.join("new.js"), "return context.inputs;").unwrap();

        let mut providers = Providers::new();
        blue::register(&mut providers, Some(tmp_dir.clone()));
        let state_path = format!("{}/state.json", tmp_dir.display());
        (providers, state_path, tmp_dir)
    }

    #[test]
    fn deploy_create() {
        let (providers, path, tmp_dir) = setup();
        let config = parse_resource_config(
            r#"
[resources.test]
type = "blue.script"
script = "test.js"
triggers_replace = { key = "value" }
"#,
        )
        .unwrap();

        let mut state = State::new();
        let plan = create_plan(&config, &state, &providers, &HashMap::new()).unwrap();
        assert_eq!(plan.steps[0].action, Action::Create);

        execute_deploy(&plan, &mut state, Path::new(&path), &providers).unwrap();
        assert!(state.resources.contains_key("test"));

        fs::remove_dir_all(&tmp_dir).ok();
    }

    #[test]
    fn deploy_delete() {
        let (providers, path, tmp_dir) = setup();
        let config = parse_resource_config("").unwrap();

        let mut state = State::new();
        state.resources.insert(
            "old".to_string(),
            ResourceState {
                resource_type: "blue.script".to_string(),
                inputs: json!({"script": "old.js"}),
                outputs: json!({}),
                depends_on: vec![],
            },
        );

        let plan = create_plan(&config, &state, &providers, &HashMap::new()).unwrap();
        execute_deploy(&plan, &mut state, Path::new(&path), &providers).unwrap();
        assert!(!state.resources.contains_key("old"));

        fs::remove_dir_all(&tmp_dir).ok();
    }

    #[test]
    fn deploy_replace() {
        let (providers, path, tmp_dir) = setup();
        let config = parse_resource_config(
            r#"
[resources.test]
type = "blue.script"
script = "new.js"
triggers_replace = { key = "value" }
"#,
        )
        .unwrap();

        let mut state = State::new();
        state.resources.insert(
            "test".to_string(),
            ResourceState {
                resource_type: "blue.script".to_string(),
                inputs: json!({"script": "old.js", "triggers_replace": {"key": "value"}}),
                outputs: json!({"old": true}),
                depends_on: vec![],
            },
        );

        let plan = create_plan(&config, &state, &providers, &HashMap::new()).unwrap();
        assert_eq!(plan.steps[0].action, Action::Replace);

        execute_deploy(&plan, &mut state, Path::new(&path), &providers).unwrap();
        assert!(state.resources.contains_key("test"));
        assert_eq!(state.resources["test"].inputs["script"], "new.js");

        fs::remove_dir_all(&tmp_dir).ok();
    }
}
