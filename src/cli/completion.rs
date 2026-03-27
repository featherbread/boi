use std::io;

use clap::CommandFactory;

use crate::child;

#[derive(clap::Args)]
pub struct Args {
    /// The shell to generate completions for
    shell: clap_complete::Shell,

    /// The command name the shell will match on
    #[arg(long)]
    #[arg(default_value = env!("CARGO_PKG_NAME"))]
    bin_name: String,
}

pub async fn main(args: Args) -> child::Result<()> {
    clap_complete::generate(
        args.shell,
        &mut crate::Cli::command(),
        args.bin_name,
        &mut io::stdout().lock(),
    );
    Ok(())
}
