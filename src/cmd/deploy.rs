use std::collections::HashMap;
use std::path::PathBuf;

use clap::Args;

use super::{compute_changeset, print_changeset, print_config, print_var_info, resolve_config, resolve_graph};
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

pub fn run(args: &DeployArgs) {
    if args.file.is_none() && args.plan.is_none() {
        eprintln!("Error: either --file or --plan must be provided");
        std::process::exit(1);
    }

    let old_state = match state::load(&args.state) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error: failed to load state file: {e}");
            std::process::exit(1);
        }
    };

    let (changeset, mut registry) = if let Some(ref plan_path) = args.plan {
        println!("Deploying from changeset: {}", plan_path.display());
        println!("  State file: {}", args.state.display());

        let cs = match state::load_changeset(plan_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Error: failed to load changeset: {e}");
                std::process::exit(1);
            }
        };

        if cs.base_serial != old_state.serial {
            eprintln!(
                "Error: changeset is stale (plan serial: {}, current state serial: {})",
                cs.base_serial, old_state.serial
            );
            eprintln!("The state has changed since this plan was created. Run 'blue plan' again.");
            std::process::exit(1);
        }

        (cs, providers::build_registry(provider::ProviderMode::Live))
    } else {
        let file = args.file.as_ref().unwrap();
        println!("Deploying from: {}", file.display());
        println!("  State file: {}", args.state.display());
        print_var_info(&args.var, args.var_file.as_deref());

        let mut resolved = resolve_config(file, &args.var, args.var_file.as_deref());
        print_config(&resolved.config);
        resolve_graph(&mut resolved);

        let cs = compute_changeset(&old_state, &mut resolved);
        (cs, resolved.registry)
    };

    print_changeset(&changeset);

    let has_changes = changeset
        .resource_changes
        .iter()
        .any(|c| !matches!(c, state::ResourceChange::Unchanged { .. }));

    if !has_changes {
        // No resource changes — just save data snapshots
        let new_state = state::State {
            version: 1,
            serial: old_state.serial,
            data: changeset.data_snapshots.clone(),
            resources: old_state.resources,
        };
        if let Err(e) = state::save(new_state, &args.state) {
            eprintln!("Error: failed to write state file: {e}");
            std::process::exit(1);
        }
        println!("\nState saved to {}", args.state.display());
        return;
    }

    let mut current_state = state::State {
        version: 1,
        serial: old_state.serial,
        data: HashMap::new(),
        resources: old_state.resources,
    };

    if let Err(e) = deploy::execute(&changeset, &mut current_state, &mut registry, &args.state) {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }

    println!("\nDeploy complete. State saved to {}", args.state.display());
}
