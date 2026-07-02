use std::ffi::OsString;

use crate::child::{self, Child};
use crate::config::Config;

#[derive(clap::Args)]
pub struct Args {
    /// The repository to use
    repository: String,

    /// Remaining args to pass directly to Borg
    args: Vec<OsString>,
}

pub async fn main(args: Args) -> child::Result<()> {
    let config = Config::load_or_die().await;
    let repo = config.get_or_die(&args.repository);

    let mut cmdline = vec![OsString::from("borg")];
    cmdline.extend(args.args);
    Child::from_cmdline(&cmdline)
        .for_borg_repo(repo)
        .complete()
        .await
}
