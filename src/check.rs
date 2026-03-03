use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};

use crate::child::{self, Child};

#[derive(clap::Args)]
pub struct Args {
    /// Only perform repository checks (chunk CRCs)
    #[arg(long)]
    repository_only: bool,

    /// Actually do the check instead of rendering a fake bar (TEMPORARY)
    #[arg(long)]
    actually_check: bool,
}

pub async fn main(args: Args) -> child::Result<()> {
    render().await;
    if !args.actually_check {
        return Ok(());
    }

    let mut cmdline = vec!["borg", "check", "-v", "--progress"];
    if args.repository_only {
        cmdline.push("--repository-only");
    }
    Child::from_cmdline(&cmdline).complete().await
}

async fn render() {
    let style = ProgressStyle::with_template("[boi] {spinner} {bar} {pos}/{len} • {wide_msg}")
        .expect("hardcoded ProgressStyle template should be valid");

    let bar = ProgressBar::no_length();
    bar.set_style(style.clone());
    bar.enable_steady_tick(Duration::from_millis(100));
    bar.set_message("Waiting for Borg to start");

    bar.tick();
    tokio::time::sleep(Duration::from_secs(2)).await;

    bar.set_message("Analyzing the stuff");
    bar.set_length(41);
    for _ in 0..41 {
        tokio::time::sleep(Duration::from_millis(195)).await;
        bar.inc(1);
    }

    bar.finish_and_clear();
    speak!("✓", "Checked all the stuff");

    // NOTE: there are multiple phases with different lengths, delineated by the msgid field.
}
