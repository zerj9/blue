use std::collections::HashMap;
use std::path::PathBuf;

use clap::Args;

use super::{
    compute_changeset, print_changeset, print_config, print_var_info, resolve_config, resolve_graph,
};
use crate::{deploy, provider, providers, state};

#[derive(Args)]
pub struct DeployArgs {
    /// Path to the TOML definition file (live plan + apply)
    #[arg(short, long, conflicts_with = "plan")]
    file: Option<PathBuf>,

    /// Path to a previously saved changeset file
    #[arg(short = 'p', long, conflicts_with_all = ["file", "var", "var_file"])]
    plan: Option<PathBuf>,

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

pub fn run(args: &DeployArgs) -> Result<(), Box<dyn std::error::Error>> {
    if args.file.is_none() && args.plan.is_none() {
        return Err("either --file or --plan must be provided".into());
    }

    let old_state = state::load(&args.state)?;

    let (changeset, mut registry, graph_output_registry) = if let Some(ref plan_path) = args.plan {
        println!("Deploying from changeset: {}", plan_path.display());
        println!("  State file: {}", args.state.display());

        let cs = state::load_changeset(plan_path)?;

        if cs.base_serial != old_state.serial {
            return Err(format!(
                "changeset is stale (plan serial: {}, current state serial: {}). Run 'blue plan' again.",
                cs.base_serial, old_state.serial
            ).into());
        }

        // When deploying from a saved plan, we need to re-resolve the graph
        // to get data/parameter/hook outputs for property resolution
        (cs, providers::build_registry(provider::ProviderMode::Live), crate::reference::OutputRegistry::new())
    } else {
        let file = args.file.as_ref().unwrap();
        println!("Deploying from: {}", file.display());
        println!("  State file: {}", args.state.display());
        print_var_info(&args.var, args.var_file.as_deref());

        let mut resolved = resolve_config(file, &args.var, args.var_file.as_deref())?;
        print_config(&resolved.config);
        resolve_graph(&mut resolved)?;

        let cs = compute_changeset(&old_state, &mut resolved)?;
        (cs, resolved.registry, resolved.output_registry)
    };

    print_changeset(&changeset);

    let has_changes = changeset
        .resource_changes
        .iter()
        .any(|c| !matches!(c, state::ResourceChange::Unchanged { .. }));

    if !has_changes {
        let new_state = state::State {
            version: 1,
            serial: old_state.serial,
            data: changeset.data_snapshots.clone(),
            resources: old_state.resources,
        };
        state::save(new_state, &args.state)?;
        println!("\nState saved to {}", args.state.display());
        return Ok(());
    }

    let mut current_state = state::State {
        version: 1,
        serial: old_state.serial,
        data: HashMap::new(),
        resources: old_state.resources,
    };

    // Load age identities for decryption if needed
    let identities = resolve_identity()?;
    let id_refs = identities.as_deref();

    deploy::execute(&changeset, &mut current_state, &mut registry, &args.state, &graph_output_registry, id_refs)?;

    println!("\nDeploy complete. State saved to {}", args.state.display());
    Ok(())
}

/// Resolve the age identity file path and load identities.
/// Returns None if no identity is configured (no secrets to decrypt).
fn resolve_identity() -> Result<Option<Vec<Box<dyn age::Identity>>>, Box<dyn std::error::Error>> {
    match std::env::var("BLUE_AGE_IDENTITY") {
        Ok(path) => {
            let ids = deploy::load_identities(std::path::Path::new(&path))?;
            Ok(Some(ids))
        }
        Err(_) => Ok(None),
    }
}
