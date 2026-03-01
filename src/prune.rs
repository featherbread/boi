use std::cmp;
use std::fmt::Display;

use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt};

use crate::child::{self, Child};

#[derive(clap::Args)]
pub struct Args {
    /// How to select archives for pruning
    #[arg(short = 'p')]
    #[arg(long)]
    #[arg(default_value_t)]
    profile: Profile,
}

#[derive(Default, Clone, clap::ValueEnum)]
enum Profile {
    #[default]
    Normal,
    Recent,
    Aggressive,
}

impl Display for Profile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Profile::Normal => "normal",
            Profile::Recent => "recent",
            Profile::Aggressive => "aggressive",
        })
    }
}

pub async fn main(args: Args) -> child::Result<()> {
    let mut base_cmdline = vec!["borg", "prune", "-v", "--list"];
    match args.profile {
        Profile::Normal => base_cmdline.extend([
            "--keep-within=24H",
            "--keep-daily=7",
            "--keep-weekly=4",
            "--keep-monthly=-1",
        ]),
        Profile::Recent => base_cmdline.extend(["--keep-last=1", "--keep-daily=-1"]),
        Profile::Aggressive => base_cmdline.extend(["--keep-daily=3"]),
    }

    let result = dry_run(&base_cmdline).await;
    result.dump_to_stderr().await;
    if result.is_prune_none() {
        return Ok(());
    }

    speak!("⚠", "Press Enter to prune snapshots for real…");
    let mut stdin = io::BufReader::new(io::stdin());
    let mut line = String::new();
    if let Ok(0) | Err(_) = stdin.read_line(&mut line).await {
        return Ok(()); // Don't treat immediate EOF or read errors as confirmation.
    }

    let mut prune_cmdline = base_cmdline.clone();
    prune_cmdline.extend(["--stats", "--progress"]);
    Child::from_cmdline(&prune_cmdline).complete().await?;

    Child::from_cmdline(&["borg", "compact", "-v", "--progress"])
        .complete()
        .await
}

async fn dry_run(base_cmdline: &[&str]) -> DryRun {
    let mut cmdline = base_cmdline.to_vec();
    cmdline.push("--dry-run");

    let Ok(output) = Child::from_cmdline(&cmdline)
        .capture_output()
        .await
        .map_err(|err| die!("Prune dry run failed to launch ({err}); you should look at that."));

    if !output.status.success() {
        dump_all_stderr(&output.stderr).await;
        die!("Prune dry run failed; you should look at that.");
    }

    DryRun::from_borg_stderr(output.stderr)
}

enum DryRun {
    PruneNone,
    PruneSome {
        stderr: Vec<u8>,
        elided: Option<DryRunElision>,
    },
}

struct DryRunElision {
    interval: String,
    count: usize,
}

impl DryRun {
    /// Parses the output of `borg prune --dry-run` to elide long lists of archives to keep.
    ///
    /// The expected format is:
    ///
    /// ```text
    /// Keeping archive (rule: daily #1): ...
    /// Would prune: ...
    /// Keeping archive (rule: weekly #1): ...
    /// Keeping archive (rule: monthly #1): ...
    /// Keeping archive (rule: monthly #2): ...
    /// ```
    ///
    /// Elision is based on long runs of the final time interval (daily, weekly, etc.) at the end
    /// of the list, or on cases where no archives are selected for pruning.
    ///
    /// As of writing, Borg doesn't have a useful machine-readable output for prune dry-runs.
    fn from_borg_stderr(mut stderr: Vec<u8>) -> DryRun {
        let Ok(output) = str::from_utf8(&stderr) else {
            return DryRun::elide_nothing(stderr);
        };

        const KEEP_PREFIX: &str = "Keeping archive (rule: ";

        // Including the newline simplifies calculations for truncating the stderr buffer.
        let lines: Vec<_> = output.split_inclusive('\n').collect();
        if lines.iter().all(|l| l.starts_with(KEEP_PREFIX)) {
            return DryRun::PruneNone;
        }

        let Some((interval, _)) = lines
            .last()
            .and_then(|l| l.strip_prefix(KEEP_PREFIX))
            .and_then(|s| s.split_once(' '))
        else {
            return DryRun::elide_nothing(stderr);
        };

        let trailing_count = lines
            .iter()
            .rev()
            .take_while(|l| {
                l.strip_prefix(KEEP_PREFIX)
                    .is_some_and(|s| s.starts_with(interval))
            })
            .count();

        let keep_count = cmp::min(lines.len(), lines.len() - trailing_count + 3);
        if keep_count == lines.len() {
            return DryRun::elide_nothing(stderr);
        }

        let interval = interval.to_owned();
        let elided_count = lines.len() - keep_count;

        stderr.truncate(lines[..keep_count].iter().map(|l| l.len()).sum());
        stderr.shrink_to_fit();

        DryRun::PruneSome {
            stderr,
            elided: Some(DryRunElision {
                interval,
                count: elided_count,
            }),
        }
    }

    fn elide_nothing(stderr: Vec<u8>) -> DryRun {
        DryRun::PruneSome {
            stderr,
            elided: None,
        }
    }

    fn is_prune_none(&self) -> bool {
        matches!(self, DryRun::PruneNone)
    }

    async fn dump_to_stderr(&self) {
        let DryRun::PruneSome { stderr, elided } = self else {
            speak!("✓", "No snapshots to prune right now.");
            return;
        };

        dump_all_stderr(stderr).await;
        if let Some(DryRunElision { interval, count }) = elided {
            speak!(
                "✱",
                "Keeping {count} more {interval} {snaps} too.",
                snaps = if *count == 1 { "snapshot" } else { "snapshots" }
            );
        }
    }
}

async fn dump_all_stderr(buf: &[u8]) {
    let mut stderr = io::stderr();
    let _ = stderr.write_all(buf).await;
    let _ = stderr.flush().await;
}
