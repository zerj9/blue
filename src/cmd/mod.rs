pub mod deploy;
pub mod destroy;
pub mod plan;
pub mod refresh;
pub mod validate;

use std::collections::HashMap;
use std::path::Path;

use crate::{config, provider, providers, state};

pub(crate) struct ResolvedConfig {
    pub config: config::Config,
    pub data_sources: HashMap<String, config::DataSource>,
    pub data_vars: HashMap<String, String>,
    pub registry: provider::ProviderRegistry,
}

pub(crate) fn resolve_config(
    file: &Path,
    var: &[String],
    var_file: Option<&Path>,
) -> ResolvedConfig {
    let cli_vars = build_cli_vars(var, var_file);

    let raw = match std::fs::read_to_string(file) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: failed to read {}: {e}", file.display());
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

    let config = match config::load(&raw, &cli_vars, &data_vars) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("Error: failed to parse {}: {e}", file.display());
            std::process::exit(1);
        }
    };

    if !config.resources.is_empty()
        && let Err(e) = registry.validate_resources(&config.resources)
    {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }

    ResolvedConfig {
        config,
        data_sources,
        data_vars,
        registry,
    }
}

pub(crate) fn build_cli_vars(var: &[String], var_file: Option<&Path>) -> HashMap<String, String> {
    let mut cli_vars = HashMap::new();

    if let Some(var_file) = var_file {
        let contents = match std::fs::read_to_string(var_file) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Error: failed to read {}: {e}", var_file.display());
                std::process::exit(1);
            }
        };
        let table: HashMap<String, toml::Value> = match toml::from_str(&contents) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("Error: failed to parse {}: {e}", var_file.display());
                std::process::exit(1);
            }
        };
        for (k, v) in table {
            cli_vars.insert(k, config::toml_value_to_string(&v));
        }
    }

    for entry in var {
        if let Some((k, v)) = entry.split_once('=') {
            cli_vars.insert(k.to_string(), v.to_string());
        } else {
            eprintln!("Error: invalid --var format: {entry} (expected KEY=VALUE)");
            std::process::exit(1);
        }
    }

    cli_vars
}

pub(crate) fn compute_changeset(
    old_state: &state::State,
    resolved: &mut ResolvedConfig,
) -> state::Changeset {
    let data_snapshots = state::snapshot_data(&resolved.data_sources, &resolved.data_vars);
    let data_changes = state::diff_data(&old_state.data, &data_snapshots);

    let resource_snapshots = state::snapshot_resources(&resolved.config.resources);
    let resource_changes = match state::diff_resources(
        &old_state.resources,
        &resource_snapshots,
        &mut resolved.registry,
    ) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: failed to diff resources: {e}");
            std::process::exit(1);
        }
    };

    state::Changeset {
        version: 1,
        base_serial: old_state.serial,
        data_snapshots,
        resource_snapshots,
        data_changes,
        resource_changes,
    }
}

pub(crate) fn print_changeset(changeset: &state::Changeset) {
    if changeset.data_changes.is_empty() {
        println!("\nNo changes detected in data sources.");
    } else {
        print_data_changes(&changeset.data_changes);
    }

    let meaningful: Vec<_> = changeset
        .resource_changes
        .iter()
        .filter(|c| !matches!(c, state::ResourceChange::Unchanged { .. }))
        .collect();
    if meaningful.is_empty() {
        println!("\nNo changes detected in resources.");
    } else {
        print_resource_changes(&changeset.resource_changes);
    }
}

pub(crate) fn print_resource_changes(changes: &[state::ResourceChange]) {
    println!("\nResource changes:");
    for change in changes {
        match change {
            state::ResourceChange::Create {
                name,
                resource_type,
                properties,
            } => {
                println!("  + {name} ({resource_type})");
                print_property_list(properties);
            }
            state::ResourceChange::Delete {
                name,
                resource_type,
            } => {
                println!("  - {name} ({resource_type})");
            }
            state::ResourceChange::Update {
                name,
                resource_type,
                changes,
            } => {
                println!("  ~ {name} ({resource_type})");
                print_property_changes(changes);
            }
            state::ResourceChange::Replace {
                name,
                resource_type,
                changes,
            } => {
                println!("  -/+ {name} ({resource_type}) (must replace)");
                print_property_changes(changes);
            }
            state::ResourceChange::Unchanged { .. } => {}
        }
    }
}

fn print_property_list(properties: &serde_json::Value) {
    let mut flat = Vec::new();
    state::flatten_json("", properties, &mut flat);
    flat.sort_by(|(a, _), (b, _)| a.cmp(b));
    for (key, value) in flat {
        println!("      + {key} = {}", json_display(&value));
    }
}

fn print_property_changes(changes: &[state::PropertyChange]) {
    for change in changes {
        match change {
            state::PropertyChange::Added { field, new_value } => {
                println!("      + {field} = {}", json_display(new_value));
            }
            state::PropertyChange::Removed { field, old_value } => {
                println!("      - {field} = {}", json_display(old_value));
            }
            state::PropertyChange::Modified {
                field,
                old_value,
                new_value,
                force_new,
            } => {
                let annotation = if *force_new {
                    " (forces replacement)"
                } else {
                    ""
                };
                println!(
                    "      ~ {field}: {} -> {}{annotation}",
                    json_display(old_value),
                    json_display(new_value)
                );
            }
        }
    }
}

fn json_display(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => format!("\"{s}\""),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

pub(crate) fn print_data_changes(changes: &[state::DataChange]) {
    println!("\nData source changes:");
    for change in changes {
        match change {
            state::DataChange::Added(name) => {
                println!("  + data.{name} (new data source)");
            }
            state::DataChange::Removed(name) => {
                println!("  - data.{name} (removed)");
            }
            state::DataChange::Changed {
                source,
                key,
                old_value,
                new_value,
            } => {
                if old_value.is_empty() {
                    println!("  ~ data.{source}.{key}: (new) -> \"{new_value}\"");
                } else if new_value.is_empty() {
                    println!("  ~ data.{source}.{key}: \"{old_value}\" -> (removed)");
                } else {
                    println!("  ~ data.{source}.{key}: \"{old_value}\" -> \"{new_value}\"");
                }
            }
            state::DataChange::FiltersChanged { source } => {
                println!("  ~ data.{source}: filters changed");
            }
        }
    }
}

pub(crate) fn print_config(config: &config::Config) {
    println!("\nParameters:");
    if config.parameters.is_empty() {
        println!("  (none)");
    } else {
        for (name, param) in &config.parameters {
            let desc = param.description.as_deref().unwrap_or("");
            let default = param
                .default
                .as_ref()
                .map(|v| v.to_string())
                .unwrap_or_else(|| "(none)".into());
            println!("  {name}: {desc} (default: {default})");
        }
    }

    println!("\nResources:");
    if config.resources.is_empty() {
        println!("  (none)");
    } else {
        for (name, resource) in &config.resources {
            println!("  {name}: {}", resource.resource_type());
        }
    }
}

pub(crate) fn print_var_info(var: &[String], var_file: Option<&Path>) {
    if let Some(var_file) = var_file {
        println!("  Var file: {}", var_file.display());
    }
    if !var.is_empty() {
        println!("  Variable overrides:");
        for v in var {
            println!("    {v}");
        }
    }
}
