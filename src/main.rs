mod config;
mod crypto;
mod deploy;
mod diff;
mod graph;
mod plan;
mod provider;
mod providers;
mod refresh;
mod resolvable;
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
        #[arg(long, default_value = "providers.toml")]
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
        #[arg(long, default_value = "providers.toml")]
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
        #[arg(long, default_value = "providers.toml")]
        providers: String,
        #[arg(long, default_value = "blue.state.json")]
        state: String,
    },
    /// Delete all managed resources
    Destroy {
        #[arg(long, default_value = "providers.toml")]
        providers: String,
        #[arg(long, default_value = "blue.state.json")]
        state: String,
    },
    /// Re-encrypt secret state values under the [encryption] recipient set
    /// from the resource config. Run after adding or removing recipients.
    Rekey {
        #[arg(short, long)]
        file: String,
        #[arg(long, default_value = "providers.toml")]
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
        Command::Plan {
            file,
            providers,
            state,
            var,
            var_file,
        } => {
            let config_dir = config_dir_from_file(&file);
            let providers = build_providers(&providers, config_dir)?;
            let config = load_resource_config(&file)?;
            config::validate_encryption(&config)?;
            let identities = crypto::load_identities()?;
            let recipients_raw = recipients_raw_from_config(&config);
            let recipients = crypto::parse_recipients(&recipients_raw)?;
            let io = state::StateIO {
                recipients: &recipients,
                identities: &identities,
                recipients_raw: &recipients_raw,
                schemas: &providers,
            };
            let state = state::read_state(Path::new(&state), &io)?;
            let params = parse_vars(&var, var_file.as_deref())?;
            let plan = plan::create_plan(&config, &state, &providers, &params)?;
            print_plan(&plan);
            Ok(())
        }
        Command::Deploy {
            file,
            providers,
            state,
            var,
            var_file,
        } => {
            let config_dir = config_dir_from_file(&file);
            let providers = build_providers(&providers, config_dir)?;
            let config = load_resource_config(&file)?;
            config::validate_encryption(&config)?;
            let identities = crypto::load_identities()?;
            let recipients_raw = recipients_raw_from_config(&config);
            let recipients = crypto::parse_recipients(&recipients_raw)?;
            let state_path = state;
            let io = state::StateIO {
                recipients: &recipients,
                identities: &identities,
                recipients_raw: &recipients_raw,
                schemas: &providers,
            };
            let mut state = state::read_state(Path::new(&state_path), &io)?;
            let params = parse_vars(&var, var_file.as_deref())?;
            let plan = plan::create_plan(&config, &state, &providers, &params)?;

            if plan.steps.is_empty() {
                println!("No changes to deploy.");
                return Ok(());
            }

            print_plan(&plan);
            deploy::execute_deploy(&plan, &mut state, Path::new(&state_path), &providers, &io)?;
            println!("Deploy complete.");
            Ok(())
        }
        Command::Refresh { providers, state } => {
            let providers = build_providers(&providers, None)?;
            let state_path = state;
            // Refresh has no config file (no [encryption] block available),
            // so the recipient set comes from `state.encrypted_with` —
            // whatever was used at the previous successful write. Rekey is
            // the only command that intentionally changes recipients.
            let identities = crypto::load_identities()?;
            let read_io = state::StateIO {
                recipients: &[],
                identities: &identities,
                recipients_raw: &[],
                schemas: &providers,
            };
            let mut state = state::read_state(Path::new(&state_path), &read_io)?;
            let recipients_raw = state.encrypted_with.clone();
            let recipients = crypto::parse_recipients(&recipients_raw)?;
            let io = state::StateIO {
                recipients: &recipients,
                identities: &identities,
                recipients_raw: &recipients_raw,
                schemas: &providers,
            };
            refresh::refresh(&mut state, Path::new(&state_path), &providers, &io)?;
            println!("Refresh complete.");
            Ok(())
        }
        Command::Destroy { providers, state } => {
            let providers = build_providers(&providers, None)?;
            let state_path = state;
            let identities = crypto::load_identities()?;
            let read_io = state::StateIO {
                recipients: &[],
                identities: &identities,
                recipients_raw: &[],
                schemas: &providers,
            };
            let mut state = state::read_state(Path::new(&state_path), &read_io)?;
            let recipients_raw = state.encrypted_with.clone();
            let recipients = crypto::parse_recipients(&recipients_raw)?;
            let io = state::StateIO {
                recipients: &recipients,
                identities: &identities,
                recipients_raw: &recipients_raw,
                schemas: &providers,
            };
            refresh::destroy(&mut state, Path::new(&state_path), &providers, &io)?;
            println!("Destroy complete.");
            Ok(())
        }
        Command::Rekey {
            file,
            providers,
            state,
        } => {
            let config_dir = config_dir_from_file(&file);
            let providers = build_providers(&providers, config_dir)?;
            let config = load_resource_config(&file)?;
            config::validate_encryption(&config)?;
            let identities = crypto::load_identities()?;
            if identities.is_empty() {
                return Err(
                    "rekey requires an identity to decrypt the existing state \
                     (set BLUE_AGE_IDENTITY or BLUE_AGE_IDENTITY_KEY)"
                        .to_string(),
                );
            }
            let recipients_raw = recipients_raw_from_config(&config);
            let recipients = crypto::parse_recipients(&recipients_raw)?;
            let state_path = state;

            // Read with identities (decrypts existing markers under whatever
            // recipients were used at the previous write). No recipients on
            // the read path — we don't encrypt anything here.
            let read_io = state::StateIO {
                recipients: &[],
                identities: &identities,
                recipients_raw: &[],
                schemas: &providers,
            };
            let mut state_data =
                state::read_state(Path::new(&state_path), &read_io)?;

            // Count secret values now (in-memory plaintext) so we can
            // report what changed without instrumenting write_state.
            let secret_count = state::count_secret_outputs(&state_data, &providers);

            // Write with current config recipients — encrypts everything
            // afresh under the new set and updates `encrypted_with`.
            let write_io = state::StateIO {
                recipients: &recipients,
                identities: &identities,
                recipients_raw: &recipients_raw,
                schemas: &providers,
            };
            state::write_state(Path::new(&state_path), &mut state_data, &write_io)?;

            println!(
                "Rekey complete. {secret_count} secret value(s) re-encrypted for \
                 {} recipient(s).",
                recipients.len()
            );
            Ok(())
        }
    }
}

/// Extract recipient strings from a parsed config, defaulting to empty
/// when no `[encryption]` block is present.
fn recipients_raw_from_config(config: &config::ResourceConfig) -> Vec<String> {
    config
        .encryption
        .as_ref()
        .map(|e| e.recipients.clone())
        .unwrap_or_default()
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
    providers_path: &str,
    config_dir: Option<std::path::PathBuf>,
) -> Result<provider::Providers, String> {
    let mut providers = provider::Providers::new();
    providers::blue::register(&mut providers, config_dir);

    let provider_file = match std::fs::read_to_string(providers_path) {
        Ok(s) => Some(config::parse_provider_config(&s)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(format!(
                "Failed to read provider config '{providers_path}': {e}"
            ));
        }
    };

    if let Some(file) = provider_file {
        // TODO: resolve [data.*] script data sources before instantiating providers
        for (name, def) in &file.providers {
            register_configured(&mut providers, name, def)?;
        }
    }

    Ok(providers)
}

/// Construct and register a provider instance from a `[name]` block in `providers.toml`.
///
/// `instance_name` is the user-chosen TOML key (e.g. `"upcloud"`, `"upcloud-us"`).
/// `def` carries the `type` field and the provider-specific config (credentials etc.).
///
/// Each provider's `register` function takes this same `(providers, instance_name, def)`
/// signature; new providers are added as match arms here.
fn register_configured(
    providers: &mut provider::Providers,
    instance_name: &str,
    def: &config::ProviderDef,
) -> Result<(), String> {
    match def.provider_type.as_str() {
        "upcloud" => providers::upcloud::register(providers, instance_name, def),
        other => Err(format!(
            "Unknown provider type '{other}' for instance '{instance_name}'"
        )),
    }
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
        println!(
            "  {symbol} {name} ({type_})",
            name = step.name,
            type_ = step.resource_type
        );
    }
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn build_providers_missing_file_is_non_fatal() {
        let path = std::env::temp_dir().join(format!("blue-no-such-{}.toml", Uuid::new_v4()));
        assert!(!path.exists());

        let providers = build_providers(path.to_str().unwrap(), None)
            .expect("missing providers.toml should not be an error");

        assert!(
            providers.resource_type("blue.script").is_some(),
            "blue provider should still be registered when providers.toml is missing"
        );
    }

    #[test]
    fn build_providers_unknown_type_errors_with_instance_name() {
        let path = std::env::temp_dir().join(format!("blue-unknown-{}.toml", Uuid::new_v4()));
        std::fs::write(
            &path,
            r#"
[my-instance]
type = "nonexistent"
"#,
        )
        .unwrap();

        let result = build_providers(path.to_str().unwrap(), None);
        let _ = std::fs::remove_file(&path);

        match result {
            Ok(_) => panic!("unknown provider type should fail"),
            Err(e) => assert!(
                e.contains("my-instance") && e.contains("nonexistent"),
                "error should name both instance and type, got: {e}"
            ),
        }
    }
}
