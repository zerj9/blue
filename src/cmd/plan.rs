use std::path::PathBuf;

use clap::Args;

use super::{
    compute_changeset, print_changeset, print_config, print_var_info, resolve_config, resolve_graph,
};
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

pub fn run(args: &PlanArgs) -> Result<(), Box<dyn std::error::Error>> {
    println!("Planning changes from: {}", args.file.display());
    println!("  State file: {}", args.state.display());
    print_var_info(&args.var, args.var_file.as_deref());

    let old_state = state::load(&args.state)?;

    let mut resolved = resolve_config(&args.file, &args.var, args.var_file.as_deref())?;
    print_config(&resolved.config);

    resolve_graph(&mut resolved)?;

    let changeset = compute_changeset(&old_state, &mut resolved)?;
    print_changeset(&changeset);

    if let Some(ref out_path) = args.out {
        state::save_changeset(&changeset, out_path)?;
        println!("\nChangeset written to {}", out_path.display());
    }

    Ok(())
}
