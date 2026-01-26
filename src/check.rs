use crate::child::{self, Child};

#[derive(clap::Args)]
pub struct Args {
    /// Only perform repository checks (chunk CRCs)
    #[arg(long)]
    repository_only: bool,
}

pub async fn main(args: Args) -> child::Result<()> {
    let mut cmdline = vec!["borg", "check", "-v", "--progress"];
    if args.repository_only {
        cmdline.push("--repository-only");
    }
    Child::from_cmdline(&cmdline).complete().await
}
