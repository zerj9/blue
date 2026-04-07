use std::path::PathBuf;

use clap::Args;

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

    // resolve_config performs full schema + ref validation
    super::resolve_config(&args.file, &args.var, args.var_file.as_deref())?;

    println!("Configuration is valid.");
    Ok(())
}
