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
        CliCommand::Check(args) => cli::check::main(args).await,
        CliCommand::Completion(args) => cli::completion::main(args).await,
        CliCommand::Prune(args) => cli::prune::main(args).await,
        CliCommand::Snapshot(args) => cli::snapshot::main(args).await,
        #[cfg(feature = "upload")]
        CliCommand::Upload => cli::upload::main().await,
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
    /// Check the consistency of the repository
    Check(cli::check::Args),

    /// Generate the autocompletion script for the specified shell
    Completion(cli::completion::Args),

    /// Thin out old backups
    Prune(cli::prune::Args),

    /// Create a new backup
    #[command(visible_aliases = ["s", "snap"])]
    Snapshot(cli::snapshot::Args),

    /// Upload the repository via a custom script on the remote host
    ///
    /// This is an anti-pattern, and is only marginally safer than keeping a single copy
    /// of your repository. As the Borg FAQ notes, any corruption or other issue in your
    /// original repository will be uploaded as-is, and your custom script may require
    /// other special safety considerations.
    ///
    /// It is hoped that a future version of boi will better support the correct pattern
    /// of maintaining fully independent repositories on separate remote hosts.
    /// At that time, this command will be removed.
    ///
    /// The authors of boi will provide absolutely no support or documentation to assist
    /// in configuring your remote repository for this command. Use of this command is
    /// AT YOUR OWN RISK of the PERMANENT LOSS of your data.
    #[cfg(feature = "upload")]
    #[command(visible_alias = "up")]
    #[command(verbatim_doc_comment)]
    Upload,
}
