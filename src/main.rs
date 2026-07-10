#![warn(clippy::undocumented_unsafe_blocks)]

use clap::{Parser, Subcommand};

#[macro_use]
mod macros;

mod borg;
mod child;
mod cli;
mod config;
mod json;
mod reporting;
mod signals;
mod snapshot;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let result = match Cli::parse().command {
        CliCommand::Borg(args) => cli::borg::main(args).await,
        CliCommand::Check(args) => cli::check::main(args).await,
        CliCommand::Completion(args) => cli::completion::main(args).await,
        CliCommand::Prune(args) => cli::prune::main(args).await,
        CliCommand::Snapshot(args) => cli::snapshot::main(args).await,
    };
    if let Err(err) = result {
        err.die();
    }
}

#[derive(Parser)]
#[command(about)]
struct Cli {
    #[command(subcommand)]
    command: CliCommand,
}

#[derive(Subcommand)]
enum CliCommand {
    /// Run a Borg CLI command for a single repo
    Borg(cli::borg::Args),

    /// Check the consistency of the repository
    Check(cli::check::Args),

    /// Generate the autocompletion script for the specified shell
    Completion(cli::completion::Args),

    /// Thin out old backups
    Prune(cli::prune::Args),

    /// Create a new backup
    #[command(visible_aliases = ["s", "snap"])]
    Snapshot(cli::snapshot::Args),
}
