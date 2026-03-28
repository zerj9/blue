use std::collections::HashMap;
use std::path::{Path, PathBuf};

use clap::Args;

use super::build_cli_vars;
use crate::{config, provider, providers, schema};

#[derive(Args)]
pub struct ValidateArgs {
    /// Path to the TOML definition file
    #[arg(short, long)]
    file: PathBuf,

    /// Variable overrides in key=value format (can be repeated)
    #[arg(long, value_name = "KEY=VALUE")]
    var: Vec<String>,

    /// Path to a variables file for overrides
    #[arg(long, value_name = "FILE")]
    var_file: Option<PathBuf>,
}

pub fn run(args: &ValidateArgs) -> Result<(), Box<dyn std::error::Error>> {
    println!("Validating: {}", args.file.display());

    let raw = std::fs::read_to_string(&args.file)
        .map_err(|e| format!("failed to read {}: {e}", args.file.display()))?;

    // Load config (resolves and validates hook script paths)
    let file_path = match args.file.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let cli_vars = build_cli_vars(&args.var, args.var_file.as_deref())?;
    let config = config::load_for_validation(&raw, &cli_vars, file_path)?;

    // Validate data source hooks
    let data_sources = &config.data;
    for (name, source) in data_sources {
        config::validate_hooks(&source.hooks, false).map_err(|e| format!("data.{name}: {e}"))?;
    }

    // Validate resource hooks
    for (name, resource) in &config.resources {
        config::validate_hooks(&resource.hooks, true)
            .map_err(|e| format!("resources.{name}: {e}"))?;
    }

    let mut registry = providers::build_registry(provider::ProviderMode::SchemaOnly);

    // Collect all provider names and pre-initialize
    let mut provider_names = std::collections::HashSet::new();
    for source in data_sources.values() {
        if let Ok((provider_name, _)) = source.provider_and_type() {
            provider_names.insert(provider_name.to_string());
        }
    }
    for resource in config.resources.values() {
        if let Ok((provider_name, _)) = resource.provider_and_type() {
            provider_names.insert(provider_name.to_string());
        }
    }
    let provider_refs: Vec<&str> = provider_names.iter().map(|s| s.as_str()).collect();
    registry.ensure_providers(&provider_refs)?;

    // Build data_schemas map
    let mut data_schemas = HashMap::new();
    for (name, source) in data_sources {
        let full_type = source.source_type();
        match registry.data_source_schema_ref(full_type)? {
            Some(s) => {
                data_schemas.insert(name.clone(), s);
            }
            None => {
                return Err(format!("data.{name}: unknown data source type '{full_type}'").into());
            }
        }
    }

    // Build resource_schemas map
    let mut resource_schemas = HashMap::new();
    for (name, resource) in &config.resources {
        let (provider_name, resource_type) = resource.provider_and_type()?;
        match registry.resource_schema_ref(resource.resource_type())? {
            Some(s) => {
                resource_schemas.insert(name.clone(), s);
            }
            None => {
                return Err(format!(
                    "resources.{name}: unknown resource type '{resource_type}' for provider '{provider_name}'"
                ).into());
            }
        }
    }

    // Build hook output maps for validation
    let mut data_hook_outputs: HashMap<String, Vec<&config::HookOutput>> = HashMap::new();
    for (name, source) in data_sources {
        let outputs: Vec<&config::HookOutput> =
            source.hooks.iter().flat_map(|h| h.outputs.iter()).collect();
        if !outputs.is_empty() {
            data_hook_outputs.insert(name.clone(), outputs);
        }
    }

    let mut resource_hook_outputs: HashMap<String, Vec<&config::HookOutput>> = HashMap::new();
    for (name, resource) in &config.resources {
        let outputs: Vec<&config::HookOutput> = resource
            .hooks
            .iter()
            .flat_map(|h| h.outputs.iter())
            .collect();
        if !outputs.is_empty() {
            resource_hook_outputs.insert(name.clone(), outputs);
        }
    }

    // Validate with reference awareness
    let ctx = schema::ValidateContext {
        data_schemas,
        resource_schemas,
        data_hook_outputs,
        resource_hook_outputs,
    };

    let mut errors = Vec::new();
    for (name, resource) in &config.resources {
        let res_schema = ctx.resource_schemas.get(name.as_str()).unwrap();
        let props = match &resource.properties {
            Some(p) => p.clone(),
            None => toml::Value::Table(Default::default()),
        };
        errors.extend(res_schema.validate_with_refs(name, &props, &ctx));
    }

    if !errors.is_empty() {
        let msg = errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        return Err(msg.into());
    }

    println!("Configuration is valid.");
    Ok(())
}
