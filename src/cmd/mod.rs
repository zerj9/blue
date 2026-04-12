pub mod deploy;
pub mod destroy;
pub mod plan;
pub mod refresh;
pub mod rekey;
pub mod validate;

use std::collections::HashMap;
use std::path::Path;

use crate::{config, graph, hooks, provider, providers, reference, schema, state};

pub(crate) struct ResolvedConfig {
    pub config: config::Config,
    pub graph: graph::DependencyGraph,
    pub output_registry: reference::OutputRegistry,
    pub registry: provider::ProviderRegistry,
}

pub(crate) fn resolve_config(
    file: &Path,
    var: &[String],
    var_file: Option<&Path>,
) -> Result<ResolvedConfig, Box<dyn std::error::Error>> {
    let cli_vars = build_cli_vars(var, var_file)?;

    let raw = std::fs::read_to_string(file)
        .map_err(|e| format!("failed to read {}: {e}", file.display()))?;

    let config_dir = match file.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let config = config::load(&raw, &cli_vars, config_dir)
        .map_err(|e| format!("failed to parse {}: {e}", file.display()))?;

    let dep_graph =
        graph::DependencyGraph::build(&config.parameters, &config.data, &config.resources)
            .map_err(|e| format!("failed to build dependency graph: {e}"))?;

    let registry = providers::build_registry(provider::ProviderMode::Live);

    // Validate encryption config
    let has_secrets = config.parameters.values().any(|p| p.secret);
    if has_secrets && config.encryption.recipients.is_empty() {
        return Err(
            "secret parameters require encryption recipients to be configured in [encryption]"
                .into(),
        );
    }

    // Validate hooks
    for (name, source) in &config.data {
        config::validate_hooks(&source.hooks, false).map_err(|e| format!("data.{name}: {e}"))?;
    }
    for (name, resource) in &config.resources {
        config::validate_hooks(&resource.hooks, true)
            .map_err(|e| format!("resources.{name}: {e}"))?;
    }

    // Full schema + ref validation
    {
        let mut schema_registry = providers::build_registry(provider::ProviderMode::SchemaOnly);

        let mut provider_names = std::collections::HashSet::new();
        for source in config.data.values() {
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
        schema_registry.ensure_providers(&provider_refs)?;

        let mut data_schemas = HashMap::new();
        for (name, source) in &config.data {
            let full_type = source.source_type();
            match schema_registry.data_source_schema_ref(full_type)? {
                Some(s) => { data_schemas.insert(name.clone(), s); }
                None => {
                    return Err(format!("data.{name}: unknown data source type '{full_type}'").into());
                }
            }
        }

        let mut resource_schemas = HashMap::new();
        for (name, resource) in &config.resources {
            let (provider_name, resource_type) = resource.provider_and_type()?;
            match schema_registry.resource_schema_ref(resource.resource_type())? {
                Some(s) => { resource_schemas.insert(name.clone(), s); }
                None => {
                    return Err(format!(
                        "resources.{name}: unknown resource type '{resource_type}' for provider '{provider_name}'"
                    ).into());
                }
            }
        }

        let mut data_hook_outputs: HashMap<String, Vec<&config::HookOutput>> = HashMap::new();
        for (name, source) in &config.data {
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

        let ctx = schema::ValidateContext {
            parameters: &config.parameters,
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
            errors.extend(res_schema.validate(name, &props, Some(&ctx)));
        }

        if !errors.is_empty() {
            let msg = errors
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            return Err(msg.into());
        }
    }

    let output_registry = reference::OutputRegistry::new();

    Ok(ResolvedConfig {
        config,
        graph: dep_graph,
        output_registry,
        registry,
    })
}

/// Walk the dependency graph in topological order, resolving data sources
/// and executing hooks just-in-time. Populates the output registry.
/// Resource outputs from the existing state are also loaded so that
/// cross-resource references (e.g. `{{ resources.X.uuid }}`) resolve
/// during planning.
pub(crate) fn resolve_graph(
    resolved: &mut ResolvedConfig,
    existing_resources: &HashMap<String, state::ResourceSnapshot>,
) -> Result<(), Box<dyn std::error::Error>> {
    let order = resolved.graph.topological_sort()?;

    for (name, kind) in &order {
        match kind {
            graph::NodeKind::Parameter => {
                resolve_parameter_node(name, resolved)?;
            }
            graph::NodeKind::Data => {
                resolve_data_node(name, resolved)?;
            }
            graph::NodeKind::Resource => {
                resolve_resource_hooks(name, resolved)?;
                if let Some(snap) = existing_resources.get(name.as_str()) {
                    if let serde_json::Value::Object(map) = &snap.outputs {
                        for (key, value) in map {
                            resolved
                                .output_registry
                                .insert("resources", name, key, value.clone());
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn resolve_data_node(
    name: &str,
    resolved: &mut ResolvedConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let source = match resolved.config.data.get(name) {
        Some(s) => s,
        None => return Ok(()),
    };

    let (provider_name, data_type) = source
        .provider_and_type()
        .map_err(|e| format!("data.{name}: {e}"))?;

    let filters = serde_json::to_value(&source.filters).map_err(|e| format!("data.{name}: {e}"))?;

    let raw_value = resolved
        .registry
        .resolve_single_data_source(provider_name, data_type, filters)
        .map_err(|e| format!("data.{name}: {e}"))?;

    // Extract schema outputs and insert into registry
    let provider_schema = resolved
        .registry
        .data_source_schema_for(provider_name, data_type);
    if let Some(s) = provider_schema {
        if let Ok(extracted) = schema::extract_outputs(&raw_value, &s.outputs) {
            resolved
                .output_registry
                .insert_outputs("data", name, &extracted);
        }
    }

    let hooks_list: Vec<config::Hook> = source.hooks.clone();
    hooks::execute_data_hooks(&hooks_list, name, &mut resolved.output_registry)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    Ok(())
}

fn resolve_resource_hooks(
    name: &str,
    resolved: &mut ResolvedConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let resource = match resolved.config.resources.get(name) {
        Some(r) => r,
        None => return Ok(()),
    };

    let hooks_list: Vec<config::Hook> = resource.hooks.clone();
    hooks::execute_safe_resource_hooks(&hooks_list, name, &mut resolved.output_registry)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    Ok(())
}

fn resolve_parameter_node(
    name: &str,
    resolved: &mut ResolvedConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let param = match resolved.config.parameters.get(name) {
        Some(p) => p.clone(),
        None => return Ok(()),
    };

    // Priority: env var > CLI override > default
    let raw_value = if let Some(ref env_key) = param.env {
        match std::env::var(env_key) {
            Ok(val) => Some(toml_value_from_string(&val, &param.param_type())),
            Err(_) => None,
        }
    } else {
        None
    };

    let raw_value = raw_value
        .or_else(|| {
            resolved
                .config
                .overrides
                .get(name)
                .map(|v| toml_value_from_string(v, &param.param_type()))
        })
        .or_else(|| param.default.clone())
        .ok_or_else(|| format!("parameter '{name}' has no value (no env, override, or default)"))?;

    // Resolve any refs in the default value (e.g., a parameter whose default references a data source)
    let json_value = toml_value_to_json(&raw_value);
    let text = json_value.to_string();
    let resolved_text = reference::Ref::resolve_all(&text, &resolved.output_registry);
    let resolved_json: serde_json::Value = serde_json::from_str(&resolved_text)
        .unwrap_or_else(|_| serde_json::Value::String(resolved_text));

    // Validate type
    if !json_type_matches_field(&param.param_type(), &resolved_json) {
        return Err(format!(
            "parameter '{name}': expected {}, got {}",
            param.param_type(),
            json_type_description(&resolved_json)
        )
        .into());
    }

    resolved
        .output_registry
        .insert("parameters", name, "value", resolved_json);
    Ok(())
}

fn toml_value_from_string(s: &str, expected: &schema::FieldType) -> toml::Value {
    match expected {
        schema::FieldType::Integer => s
            .parse::<i64>()
            .map(toml::Value::Integer)
            .unwrap_or_else(|_| toml::Value::String(s.to_string())),
        schema::FieldType::Float => s
            .parse::<f64>()
            .map(toml::Value::Float)
            .unwrap_or_else(|_| toml::Value::String(s.to_string())),
        schema::FieldType::Boolean => s
            .parse::<bool>()
            .map(toml::Value::Boolean)
            .unwrap_or_else(|_| toml::Value::String(s.to_string())),
        _ => toml::Value::String(s.to_string()),
    }
}

fn toml_value_to_json(value: &toml::Value) -> serde_json::Value {
    match value {
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Integer(n) => serde_json::json!(n),
        toml::Value::Float(f) => serde_json::json!(f),
        toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
        toml::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(toml_value_to_json).collect())
        }
        toml::Value::Table(t) => {
            let map: serde_json::Map<String, serde_json::Value> =
                t.iter().map(|(k, v)| (k.clone(), toml_value_to_json(v))).collect();
            serde_json::Value::Object(map)
        }
        toml::Value::Datetime(d) => serde_json::Value::String(d.to_string()),
    }
}

fn json_type_matches_field(expected: &schema::FieldType, value: &serde_json::Value) -> bool {
    match expected {
        schema::FieldType::String => matches!(value, serde_json::Value::String(_)),
        schema::FieldType::Integer => {
            matches!(value, serde_json::Value::Number(n) if n.is_i64() || n.is_u64())
        }
        schema::FieldType::Float => matches!(value, serde_json::Value::Number(_)),
        schema::FieldType::Boolean => matches!(value, serde_json::Value::Bool(_)),
        schema::FieldType::Array => matches!(value, serde_json::Value::Array(_)),
    }
}

fn json_type_description(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::String(_) => "string",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
        serde_json::Value::Null => "null",
    }
}

fn secret_param_names(
    parameters: &HashMap<String, config::Parameter>,
) -> std::collections::HashSet<String> {
    parameters
        .iter()
        .filter(|(_, p)| p.secret)
        .map(|(name, _)| name.clone())
        .collect()
}

pub(crate) fn build_cli_vars(
    var: &[String],
    var_file: Option<&Path>,
) -> Result<HashMap<String, String>, Box<dyn std::error::Error>> {
    let mut cli_vars = HashMap::new();

    if let Some(var_file) = var_file {
        let contents = std::fs::read_to_string(var_file)
            .map_err(|e| format!("failed to read {}: {e}", var_file.display()))?;
        let table: HashMap<String, toml::Value> = toml::from_str(&contents)
            .map_err(|e| format!("failed to parse {}: {e}", var_file.display()))?;
        for (k, v) in table {
            cli_vars.insert(k, config::toml_value_to_string(&v));
        }
    }

    for entry in var {
        if let Some((k, v)) = entry.split_once('=') {
            cli_vars.insert(k.to_string(), v.to_string());
        } else {
            return Err(format!("invalid --var format: {entry} (expected KEY=VALUE)").into());
        }
    }

    Ok(cli_vars)
}

pub(crate) fn compute_changeset(
    old_state: &state::State,
    resolved: &mut ResolvedConfig,
) -> Result<state::Changeset, Box<dyn std::error::Error>> {
    let data_vars = resolved.output_registry.to_data_vars();
    let data_snapshots = state::snapshot_data(&resolved.config.data, &data_vars);
    let data_changes = state::diff_data(&old_state.data, &data_snapshots);

    // Resolve resource properties using the output registry before snapshotting
    let secret_params = secret_param_names(&resolved.config.parameters);
    let resource_snapshots = state::snapshot_resources_resolved(
        &resolved.config.resources,
        &resolved.output_registry,
        &secret_params,
        &resolved.config.encryption.recipients,
        &old_state.resources,
    );
    let resource_changes = state::diff_resources(
        &old_state.resources,
        &resource_snapshots,
        &mut resolved.registry,
    )?;

    Ok(state::Changeset {
        version: 1,
        base_serial: old_state.serial,
        data_snapshots,
        resource_snapshots,
        data_changes,
        resource_changes,
    })
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
            state::PropertyChange::Added {
                field,
                new_value,
                force_new,
            } => {
                let annotation = if *force_new {
                    " (forces replacement)"
                } else {
                    ""
                };
                println!(
                    "      + {field}: (not set) -> {}{annotation}",
                    json_display(new_value)
                );
            }
            state::PropertyChange::Removed { field, old_value } => {
                println!(
                    "      - {field}: {} -> (removed)",
                    json_display(old_value)
                );
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
                let old_str = if old_value.is_null() {
                    "(none)".to_string()
                } else {
                    old_value.to_string()
                };
                let new_str = if new_value.is_null() {
                    "(none)".to_string()
                } else {
                    new_value.to_string()
                };

                if old_value.is_null() {
                    println!("  ~ data.{source}.{key}: (new) -> {new_str}");
                } else if new_value.is_null() {
                    println!("  ~ data.{source}.{key}: {old_str} -> (removed)");
                } else {
                    println!("  ~ data.{source}.{key}: {old_str} -> {new_str}");
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
