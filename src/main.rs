mod cmd;
mod config;
mod deploy;
mod graph;
mod hooks;
mod provider;
mod providers;
mod reference;
mod schema;
mod state;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "blue",
    version,
    about = "Infrastructure deployment tool for UpCloud"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate a configuration file against provider schemas
    Validate(cmd::validate::ValidateArgs),
    /// Show what would change without applying anything
    Plan(cmd::plan::PlanArgs),
    /// Deploy resources
    Deploy(cmd::deploy::DeployArgs),
    /// Destroy previously deployed resources
    Destroy(cmd::destroy::DestroyArgs),
    /// Re-resolve data sources and update the state file
    Refresh(cmd::refresh::RefreshArgs),
    /// Re-encrypt state secrets with the current recipient list
    Rekey(cmd::rekey::RekeyArgs),
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Command::Validate(args) => cmd::validate::run(&args),
        Command::Plan(args) => cmd::plan::run(&args),
        Command::Deploy(args) => cmd::deploy::run(&args),
        Command::Destroy(args) => cmd::destroy::run(&args),
        Command::Refresh(args) => cmd::refresh::run(&args),
        Command::Rekey(args) => cmd::rekey::run(&args),
    };

    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
