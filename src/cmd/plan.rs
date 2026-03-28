use std::path::PathBuf;

use clap::Args;
use tokio::runtime::Runtime;

use super::{compute_changeset, print_changeset, print_config, print_var_info, resolve_config};
use crate::state;

#[derive(Args)]
pub struct PlanArgs {
    /// Path to the TOML definition file
    #[arg(short, long)]
    file: PathBuf,

    /// Write changeset to this file
    #[arg(short, long)]
    out: Option<PathBuf>,

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

pub fn run(args: &PlanArgs) {
    println!("Planning changes from: {}", args.file.display());
    println!("  State file: {}", args.state.display());
    print_var_info(&args.var, args.var_file.as_deref());

    let old_state = match state::load(&args.state) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error: failed to load state file: {e}");
            std::process::exit(1);
        }
    };

    let mut resolved = resolve_config(&args.file, &args.var, args.var_file.as_deref());
    print_config(&resolved.config);

    // Execute safe hooks during planning
    let rt = Runtime::new().unwrap();
    let plan_outputs = rt.block_on(async {
        state::execute_plan_hooks(&resolved.hook_registry, &old_state).await
    });
    
    // Merge plan outputs into resolved vars for accurate planning
    for (key, value) in plan_outputs {
        resolved.data_vars.insert(key, value);
    }

    let changeset = compute_changeset(&old_state, &mut resolved);
    print_changeset(&changeset);

    if let Some(ref out_path) = args.out {
        if let Err(e) = state::save_changeset(&changeset, out_path) {
            eprintln!("Error: failed to write changeset: {e}");
            std::process::exit(1);
        }
        println!("\nChangeset written to {}", out_path.display());
    }
}
