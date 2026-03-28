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

pub fn run(args: &ValidateArgs) {
    println!("Validating: {}", args.file.display());

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
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    };

    // Validate data source hooks
    let file_path = args.file.parent().unwrap_or_else(|| Path::new(""));
    for (name, source) in &data_sources {
        if let Err(e) = config::validate_hooks(&source.hooks, file_path.to_str().unwrap_or("."), false) {
            eprintln!("Error: data.{name}: {e}");
            std::process::exit(1);
        }
    }

    // Load config deferring both data and resource references
    let cli_vars = build_cli_vars(&args.var, args.var_file.as_deref());
    let config = match config::load_for_validation(&raw, &cli_vars) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    };

    // Validate resource hooks
    for (name, resource) in &config.resources {
        if let Err(e) = config::validate_hooks(&resource.hooks, file_path.to_str().unwrap_or("."), true) {
            eprintln!("Error: resources.{name}: {e}");
            std::process::exit(1);
        }
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
    if let Err(e) = registry.ensure_providers(&provider_refs) {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }

    // Build data_schemas map
    let mut data_schemas = HashMap::new();
    for (name, source) in &data_sources {
        let full_type = source.source_type();
        match registry.data_source_schema_ref(full_type) {
            Ok(Some(s)) => {
                data_schemas.insert(name.clone(), s);
            }
            Ok(None) => {
                eprintln!("Error: data.{name}: unknown data source type '{full_type}'");
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("Error: data.{name}: {e}");
                std::process::exit(1);
            }
        }
    }

    // Build resource_schemas map
    let mut resource_schemas = HashMap::new();
    for (name, resource) in &config.resources {
        let (provider_name, resource_type) = match resource.provider_and_type() {
            Ok(pt) => pt,
            Err(e) => {
                eprintln!("Error: resources.{name}: {e}");
                std::process::exit(1);
            }
        };
        match registry.resource_schema_ref(resource.resource_type()) {
            Ok(Some(s)) => {
                resource_schemas.insert(name.clone(), s);
            }
            Ok(None) => {
                eprintln!(
                    "Error: resources.{name}: unknown resource type '{resource_type}' for provider '{provider_name}'"
                );
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("Error: resources.{name}: {e}");
                std::process::exit(1);
            }
        }
    }

    // Validate with reference awareness
    let ctx = schema::ValidateContext {
        data_schemas,
        resource_schemas,
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
        eprintln!("Error: {msg}");
        std::process::exit(1);
    }

    println!("Configuration is valid.");
}
