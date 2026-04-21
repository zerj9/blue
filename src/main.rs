mod config;
mod deploy;
mod diff;
mod graph;
mod plan;
mod provider;
mod providers;
mod refresh;
mod schema;
mod state;
mod template;
mod types;

use std::collections::HashMap;
use std::path::Path;

use clap::{Parser, Subcommand};
use serde_json::Value;

#[derive(Parser)]
#[command(name = "blue", about = "Infrastructure as Code in TOML")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Build the dependency graph, diff config against state, and produce a plan
    Plan {
        #[arg(short, long)]
        file: String,
        #[arg(long, default_value = "blue.providers.toml")]
        providers: String,
        #[arg(long, default_value = "blue.state.json")]
        state: String,
        #[arg(long, value_name = "KEY=VALUE")]
        var: Vec<String>,
        #[arg(long, value_name = "FILE")]
        var_file: Option<String>,
    },
    /// Execute a plan to create, update, or delete resources
    Deploy {
        #[arg(short, long)]
        file: String,
        #[arg(long, default_value = "blue.providers.toml")]
        providers: String,
        #[arg(long, default_value = "blue.state.json")]
        state: String,
        #[arg(long, value_name = "KEY=VALUE")]
        var: Vec<String>,
        #[arg(long, value_name = "FILE")]
        var_file: Option<String>,
    },
    /// Update state with live values from providers
    Refresh {
        #[arg(long, default_value = "blue.providers.toml")]
        providers: String,
        #[arg(long, default_value = "blue.state.json")]
        state: String,
    },
    /// Delete all managed resources
    Destroy {
        #[arg(long, default_value = "blue.providers.toml")]
        providers: String,
        #[arg(long, default_value = "blue.state.json")]
        state: String,
    },
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        Command::Plan { file, providers, state, var, var_file } => {
            let config_dir = config_dir_from_file(&file);
            let providers = build_providers(&providers, config_dir)?;
            let config = load_resource_config(&file)?;
            let state = state::read_state(Path::new(&state))?;
            let params = parse_vars(&var, var_file.as_deref())?;
            let plan = plan::create_plan(&config, &state, &providers, &params)?;
            print_plan(&plan);
            Ok(())
        }
        Command::Deploy { file, providers, state, var, var_file } => {
            let config_dir = config_dir_from_file(&file);
            let providers = build_providers(&providers, config_dir)?;
            let config = load_resource_config(&file)?;
            let state_path = state;
            let mut state = state::read_state(Path::new(&state_path))?;
            let params = parse_vars(&var, var_file.as_deref())?;
            let plan = plan::create_plan(&config, &state, &providers, &params)?;

            if plan.steps.is_empty() {
                println!("No changes to deploy.");
                return Ok(());
            }

            print_plan(&plan);
            deploy::execute_deploy(&plan, &mut state, Path::new(&state_path), &providers)?;
            println!("Deploy complete.");
            Ok(())
        }
        Command::Refresh { providers, state } => {
            let providers = build_providers(&providers, None)?;
            let state_path = state;
            let mut state = state::read_state(Path::new(&state_path))?;
            refresh::refresh(&mut state, Path::new(&state_path), &providers)?;
            println!("Refresh complete.");
            Ok(())
        }
        Command::Destroy { providers, state } => {
            let providers = build_providers(&providers, None)?;
            let state_path = state;
            let mut state = state::read_state(Path::new(&state_path))?;
            refresh::destroy(&mut state, Path::new(&state_path), &providers)?;
            println!("Destroy complete.");
            Ok(())
        }
    }
}

fn config_dir_from_file(file: &str) -> Option<std::path::PathBuf> {
    Path::new(file).parent().map(|p| {
        if p.as_os_str().is_empty() {
            std::path::PathBuf::from(".")
        } else {
            p.to_path_buf()
        }
    })
}

fn build_providers(
    _providers_path: &str,
    config_dir: Option<std::path::PathBuf>,
) -> Result<provider::Providers, String> {
    let mut providers = provider::Providers::new();
    providers::blue::register(&mut providers, config_dir);
    // TODO: parse providers config, register external providers
    Ok(providers)
}

fn load_resource_config(path: &str) -> Result<config::ResourceConfig, String> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read config file '{path}': {e}"))?;
    config::parse_resource_config(&contents)
}

fn parse_vars(vars: &[String], var_file: Option<&str>) -> Result<HashMap<String, Value>, String> {
    let mut params = HashMap::new();

    if let Some(file) = var_file {
        let contents = std::fs::read_to_string(file)
            .map_err(|e| format!("Failed to read var file '{file}': {e}"))?;
        let table: toml::Table = toml::from_str(&contents)
            .map_err(|e| format!("Failed to parse var file '{file}': {e}"))?;
        for (k, v) in table {
            params.insert(k, toml_to_json(v));
        }
    }

    for var in vars {
        let (key, value) = var
            .split_once('=')
            .ok_or_else(|| format!("Invalid --var format: '{var}', expected KEY=VALUE"))?;
        params.insert(key.to_string(), Value::String(value.to_string()));
    }

    Ok(params)
}

fn toml_to_json(value: toml::Value) -> Value {
    match value {
        toml::Value::String(s) => Value::String(s),
        toml::Value::Integer(i) => Value::Number(i.into()),
        toml::Value::Float(f) => serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        toml::Value::Boolean(b) => Value::Bool(b),
        toml::Value::Array(arr) => Value::Array(arr.into_iter().map(toml_to_json).collect()),
        toml::Value::Table(t) => {
            Value::Object(t.into_iter().map(|(k, v)| (k, toml_to_json(v))).collect())
        }
        toml::Value::Datetime(dt) => Value::String(dt.to_string()),
    }
}

fn print_plan(plan: &plan::Plan) {
    if plan.steps.is_empty() {
        println!("No changes.");
        return;
    }

    println!("\nPlan: {} action(s)\n", plan.steps.len());
    for step in &plan.steps {
        let symbol = match &step.action {
            types::Action::Create => "+",
            types::Action::Update => "~",
            types::Action::Replace => "-/+",
            types::Action::Delete => "-",
            types::Action::Unchanged => " ",
        };
        println!("  {symbol} {name} ({type_})", name = step.name, type_ = step.resource_type);
    }
    println!();
}
