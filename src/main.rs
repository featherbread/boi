#![warn(clippy::undocumented_unsafe_blocks)]

use std::io;

use clap::{Parser, Subcommand};

#[macro_use]
mod macros;

mod child;
mod signals;

mod check;
mod completion;
mod prune;
mod snapshot;
#[cfg(feature = "upload")]
mod upload;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let result = match Cli::parse().command {
        CliCommand::Check(args) => check::main(args).await,
        CliCommand::Completion(args) => completion::main(args).await,
        CliCommand::Prune(args) => prune::main(args).await,
        CliCommand::Snapshot(args) => snapshot::main(args).await,
        #[cfg(feature = "upload")]
        CliCommand::Upload => upload::main().await,
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
    Check(check::Args),

    /// Generate the autocompletion script for the specified shell
    Completion(completion::Args),

    /// Thin out old backups
    Prune(prune::Args),

    /// Create a new backup
    #[command(visible_aliases = ["s", "snap"])]
    Snapshot(snapshot::Args),

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

/// Maps `-1` returned by a libc call to the [`io::Error`] representing `errno`.
fn result_of<F, T>(call: F) -> io::Result<T>
where
    F: FnOnce() -> T,
    T: From<i8> + Eq,
{
    // POSIX has lots of typedefs like pid_t that map to int on the platforms I care about, but
    // _could_ map to something else (e.g. pid_t only has to fit into a long). The libc crate
    // translates these typedefs to Rust type aliases, so it's easy to get sloppy about
    // interchanging them with libc::c_int or even the other typedefs. This function is carefully
    // defined to avoid such footguns.
    //
    // https://github.com/rust-lang/miri/blob/2a69c39b8a65b3ee4c00078925b92770dadf685d/tests/utils/libc.rs#L8
    // is a loose inspiration, but this version accepts negative values other than -1 as valid.
    // My original idea used T: Into<libc::c_int>, but the Miri folks' approach of converting from
    // the smallest width _to_ the output type is clearly smarter.
    match call() {
        ret if ret.eq(&T::from(-1i8)) => Err(io::Error::last_os_error()),
        ret => Ok(ret),
    }
}
