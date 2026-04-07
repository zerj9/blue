use std::path::PathBuf;

use clap::Args;

use super::{print_data_changes, print_var_info, resolve_config, resolve_graph};
use crate::{provider, schema, state};

#[derive(Args)]
pub struct RefreshArgs {
    /// Path to the TOML definition file
    #[arg(short, long)]
    file: PathBuf,

    /// Variable overrides in key=value format (can be repeated)
    #[arg(long, value_name = "KEY=VALUE")]
    var: Vec<String>,

    /// Path to a variables file for overrides
    #[arg(long, value_name = "FILE")]
    var_file: Option<PathBuf>,

    /// Path to the state file
    #[arg(long, default_value = "blue.state.json")]
    state: PathBuf,
}

pub fn run(args: &RefreshArgs) -> Result<(), Box<dyn std::error::Error>> {
    println!("Refreshing state from: {}", args.file.display());
    println!("  State file: {}", args.state.display());
    print_var_info(&args.var, args.var_file.as_deref());

    let old_state = state::load(&args.state)?;

    // Resolve data sources via graph-driven resolution
    let mut resolved = resolve_config(&args.file, &args.var, args.var_file.as_deref())?;
    resolve_graph(&mut resolved)?;

    let data_vars = resolved.output_registry.to_data_vars();
    let new_data = state::snapshot_data(&resolved.config.data, &data_vars);
    let data_changes = state::diff_data(&old_state.data, &new_data);

    if data_changes.is_empty() {
        println!("\nData sources: no changes.");
    } else {
        print_data_changes(&data_changes);
    }

    // Refresh resources with Ready status
    let mut new_resources = old_state.resources.clone();
    for (name, snap) in &old_state.resources {
        if snap.status == state::ResourceStatus::Ready {
            match resolved
                .registry
                .read_resource(&snap.resource_type, &snap.outputs)
            {
                Ok(provider::OperationResult::Complete { outputs }) => {
                    let schema = resolved
                        .registry
                        .resource_schema(&snap.resource_type)
                        .ok()
                        .flatten();
                    let extracted = match schema {
                        Some(s) => {
                            let map =
                                schema::extract_outputs(&outputs, &s.outputs).unwrap_or_default();
                            let obj: serde_json::Map<String, serde_json::Value> =
                                map.into_iter().collect();
                            serde_json::Value::Object(obj)
                        }
                        None => outputs,
                    };
                    new_resources.get_mut(name).unwrap().outputs = extracted;
                    println!("  {name}: refreshed");
                }
                Ok(provider::OperationResult::Failed { error }) => {
                    eprintln!("  {name}: refresh failed: {error}");
                }
                Ok(provider::OperationResult::InProgress { .. }) => {
                    println!("  {name}: still in progress");
                }
                Ok(provider::OperationResult::Updating { .. }) => {
                    println!("  {name}: update in progress");
                }
                Err(e) => {
                    eprintln!("  {name}: refresh error: {e}");
                }
            }
        }
    }

    // Update properties from config for resources already in state
    if !new_resources.is_empty() {
        let secret_params = super::secret_param_names(&resolved.config.parameters);
        let config_snapshots = state::snapshot_resources_resolved(
            &resolved.config.resources,
            &resolved.output_registry,
            &secret_params,
        );
        for (name, snap) in &mut new_resources {
            if let Some(config_snap) = config_snapshots.get(name) {
                snap.properties = config_snap.properties.clone();
            }
        }
    }

    let new_state = state::State {
        version: 1,
        serial: old_state.serial,
        data: new_data,
        resources: new_resources,
    };

    state::save(new_state, &args.state)?;

    println!("\nState refreshed and saved to {}", args.state.display());
    Ok(())
}
