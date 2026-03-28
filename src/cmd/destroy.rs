use std::path::PathBuf;

use clap::Args;

use super::print_var_info;
use crate::{deploy, graph, provider, providers, state};

#[derive(Args)]
pub struct DestroyArgs {
    /// Path to the TOML definition file
    #[arg(short, long)]
    file: PathBuf,

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

pub fn run(args: &DestroyArgs) -> Result<(), Box<dyn std::error::Error>> {
    println!("Destroying resources from: {}", args.file.display());
    println!("  State file: {}", args.state.display());
    print_var_info(&args.var, args.var_file.as_deref());

    let mut old_state = state::load(&args.state)?;

    if old_state.resources.is_empty() {
        println!("\nNo resources in state to destroy.");
        return Ok(());
    }

    println!("\nResources to destroy:");
    for (name, snap) in &old_state.resources {
        println!("  - {name} ({})", snap.resource_type);
    }

    let graph = graph::DependencyGraph::build_from_snapshots(&old_state.resources)?;
    let order = graph.topological_sort_names()?;

    let mut registry = providers::build_registry(provider::ProviderMode::Live);

    // Delete in reverse dependency order
    for name in order.iter().rev() {
        if old_state.resources.contains_key(name) {
            deploy::delete_resource(name, &mut old_state, &mut registry, &args.state)?;
        }
    }

    println!(
        "\nAll resources destroyed. State saved to {}",
        args.state.display()
    );
    Ok(())
}
