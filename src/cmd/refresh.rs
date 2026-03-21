use std::collections::HashMap;
use std::path::PathBuf;

use clap::Args;

use super::{build_cli_vars, print_data_changes, print_var_info};
use crate::{config, provider, providers, schema, state};

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

pub fn run(args: &RefreshArgs) {
    println!("Refreshing state from: {}", args.file.display());
    println!("  State file: {}", args.state.display());
    print_var_info(&args.var, args.var_file.as_deref());

    let old_state = match state::load(&args.state) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error: failed to load state file: {e}");
            std::process::exit(1);
        }
    };

    // Re-resolve data sources
    let cli_vars = build_cli_vars(&args.var, args.var_file.as_deref());

    let raw = match std::fs::read_to_string(&args.file) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: failed to read {}: {e}", args.file.display());
            std::process::exit(1);
        }
    };

    let data_sources = match config::extract_data_sources(&raw) {
        Ok(ds) => ds,
        Err(e) => {
            eprintln!("Error: failed to parse data sources: {e}");
            std::process::exit(1);
        }
    };

    let mut registry = providers::build_registry(provider::ProviderMode::Live);

    let data_vars = if data_sources.is_empty() {
        HashMap::new()
    } else {
        match registry.resolve_data_sources(&data_sources) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("Error: failed to resolve data sources: {e}");
                std::process::exit(1);
            }
        }
    };

    let new_data = state::snapshot_data(&data_sources, &data_vars);
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
            match registry.read_resource(&snap.resource_type, &snap.outputs) {
                Ok(provider::OperationResult::Complete { outputs }) => {
                    let schema = registry.resource_schema(&snap.resource_type).ok().flatten();
                    let extracted = match schema {
                        Some(s) => {
                            let map =
                                schema::extract_outputs(&outputs, &s.outputs).unwrap_or_default();
                            let obj: serde_json::Map<String, serde_json::Value> = map
                                .into_iter()
                                .map(|(k, v)| (k, serde_json::Value::String(v)))
                                .collect();
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
                Err(e) => {
                    eprintln!("  {name}: refresh error: {e}");
                }
            }
        }
    }

    // Update properties from config for resources already in state
    if !new_resources.is_empty() {
        let config = match config::load(&raw, &cli_vars, &data_vars) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Error: failed to parse {}: {e}", args.file.display());
                std::process::exit(1);
            }
        };

        let config_snapshots = state::snapshot_resources(&config.resources);
        for (name, snap) in &mut new_resources {
            if let Some(config_snap) = config_snapshots.get(name) {
                snap.properties = config_snap.properties.clone();
            }
        }
    }

    let final_resources = new_resources;

    let new_state = state::State {
        version: 1,
        serial: old_state.serial,
        data: new_data,
        resources: final_resources,
    };

    if let Err(e) = state::save(new_state, &args.state) {
        eprintln!("Error: failed to write state file: {e}");
        std::process::exit(1);
    }

    println!("\nState refreshed and saved to {}", args.state.display());
}
