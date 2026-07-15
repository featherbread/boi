use std::cmp;
use std::env;
use std::fmt::{self, Display};
use std::iter;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use futures::StreamExt;
use indicatif::HumanBytes;

use crate::borg::{self, ArchiveStats, Event, Progress};
use crate::child::{self, Child};
use crate::config::{Config, RepoConfig};
use crate::reporting::{RepoReporter, Reporter, Widget};

#[cfg(boi_has_driver = "apfs")]
use crate::drivers::apfs;
#[cfg(boi_has_driver = "none")]
use crate::drivers::none;

#[derive(clap::Args)]
pub struct Args {
    /// Specific repositories to snapshot to
    repositories: Vec<String>,

    /// How to snapshot the home directory
    #[arg(long)]
    #[arg(default_value_t)]
    driver: DriverKind,

    #[cfg(boi_has_driver = "apfs")]
    #[command(flatten)]
    apfs: apfs::Args,
}

#[derive(Clone, clap::ValueEnum)]
pub enum DriverKind {
    #[cfg(boi_has_driver = "apfs")]
    Apfs,
    #[cfg(boi_has_driver = "none")]
    None,
}

// It's _possible_ to derive this with complicated #[cfg_attr(…, default)] attributes on the
// variants, but not nearly as clean as this linear flow.
#[expect(clippy::derivable_impls)]
impl Default for DriverKind {
    fn default() -> Self {
        cfg_select! {
            boi_has_driver = "apfs" => DriverKind::Apfs,
            boi_has_driver = "none" => DriverKind::None,
            _ => compile_error!("no snapshot drivers enabled by cfg(boi_has_driver)"),
        }
    }
}

impl Display for DriverKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            #[cfg(boi_has_driver = "apfs")]
            DriverKind::Apfs => "apfs",
            #[cfg(boi_has_driver = "none")]
            DriverKind::None => "none",
        })
    }
}

pub async fn main(args: Args) -> child::Result<()> {
    let config = Config::load_or_die().await;
    let repos: Vec<_> = if args.repositories.is_empty() {
        config.repos().collect()
    } else {
        config.select_repos_or_die(&args.repositories).collect()
    };

    let Ok(ts) = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) else {
        die!("System time is before the UNIX epoch; what are you doing?!?");
    };

    let run = async move |path: PathBuf| {
        let all_stats: Vec<SharedArchiveStats> = repos.iter().map(|_| Default::default()).collect();

        let mut reporter =
            Reporter::new(Widget::new(ArchiveStatsSummaryRunning(all_stats.clone())));

        let tasks: Vec<_> = iter::zip(repos, &all_stats)
            .map(|((name, repo), stats)| Task {
                ts,
                path: path.clone(),
                repo: repo.clone(),
                stats: Arc::clone(stats),
                reporter: reporter.add_repo(
                    name.to_owned(),
                    Widget::new(ArchiveStatsDisplay(Arc::clone(stats))),
                ),
            })
            .collect();

        let reporter = reporter.lock_repos();

        let spawns: Vec<_> = tasks
            .into_iter()
            .map(|task| tokio::spawn(task.run()))
            .collect();

        let mut child_err = None;
        for spawn in spawns {
            let result = spawn.await.unwrap();
            if let (Err(err), None) = (result, &mut child_err) {
                child_err = Some(err);
            }
        }

        match child_err {
            None => {
                let stats = ArchiveStatsSummary(&all_stats);
                reporter.succeed(format_args!("Archived {stats}."));
                Ok(())
            }
            Some(err) => {
                reporter.fail("Failed to archive to all repos.");
                Err(err)
            }
        }
    };

    match args.driver {
        #[cfg(boi_has_driver = "apfs")]
        DriverKind::Apfs => apfs::with_backup_root(args.apfs, run).await,
        #[cfg(boi_has_driver = "none")]
        DriverKind::None => none::with_backup_root(run).await,
    }
}

struct Task {
    ts: Duration,
    path: PathBuf,
    repo: RepoConfig,
    stats: SharedArchiveStats,
    reporter: RepoReporter,
}

impl Task {
    async fn run(mut self) -> child::Result<()> {
        let backup_spec = format!(
            "{url}::{sec}",
            url = self.repo.repo_url(),
            sec = self.ts.as_secs()
        );
        let cmdline = [
            "borg",
            "create",
            "--exclude-from=.borg-excludes",
            "--exclude-caches",
            "--compression=auto,zstd,19",
            "--progress",
            "--log-json",
            "--json",
            &backup_spec,
            ".",
        ];
        let (mut spawn, output) = Child::from_cmdline(&cmdline)
            .for_borg_repo(&self.repo)
            .working_directory(self.path)
            .spawn_with_output()
            .await?;

        let mut archive_complete_event = None;
        let mut event_stream = borg::stream(output);
        while let Some(event) = event_stream.next().await {
            match event {
                Ok(Event::ProgressMessage(msg)) => {
                    self.reporter.post_message(msg);
                }
                Ok(Event::ArchiveProgress(Progress::Finished)) => {
                    self.reporter.post_message("Finished archiving files");
                }
                Ok(Event::ArchiveProgress(Progress::Running(progress))) => {
                    *self.stats.write().unwrap() = progress.stats;
                    self.reporter.post_message(progress.path);
                }
                Ok(Event::ArchiveComplete(event)) => {
                    archive_complete_event = Some(event);
                }
                Ok(Event::LogMessage(msg)) => {
                    self.reporter.suspend(|| speak!("⚑", "{}", msg.message));
                }
                event => match self.reporter.post_unhandled_event(event) {
                    ControlFlow::Continue(()) => {}
                    ControlFlow::Break(()) => break,
                },
            }
        }

        let mut duration = None;
        if let Some(event) = archive_complete_event {
            duration = Some(event.duration);
            *self.stats.write().unwrap() = event.stats;
        }

        let child_result = self
            .reporter
            .wait_for_spawn(&mut spawn, "Waiting for Borg to exit…")
            .await;

        let reporter = self.reporter;
        match &child_result {
            Ok(()) => reporter.succeed(format_args!(
                "Created archive{suffix}",
                suffix = duration
                    .map(|d| format!(" in {d} seconds"))
                    .unwrap_or_default(),
            )),
            Err(err) => reporter.fail_from_child(err),
        };

        child_result
    }
}

type SharedArchiveStats = Arc<RwLock<ArchiveStats>>;

pub struct ArchiveStatsDisplay(SharedArchiveStats);

impl Display for ArchiveStatsDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.read().unwrap().fmt(f)
    }
}

pub struct ArchiveStatsSummaryRunning(Vec<SharedArchiveStats>);

impl Display for ArchiveStatsSummaryRunning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let summary = ArchiveStatsSummary(&self.0);
        write!(f, "Archiving {summary}…")
    }
}

pub struct ArchiveStatsSummary<'s>(&'s [SharedArchiveStats]);

impl<'s> Display for ArchiveStatsSummary<'s> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (max_nfiles, max_size) = self
            .0
            .iter()
            .map(|s| {
                let s = s.read().unwrap();
                (s.nfiles, s.original_size)
            })
            .reduce(|(na, sa), (nb, sb)| (cmp::max(na, nb), cmp::max(sa, sb)))
            .unwrap_or_default();

        if (max_nfiles, max_size) == (0, 0) {
            write!(f, "files")
        } else {
            write!(
                f,
                "{size} in {nfiles} file{s}",
                size = HumanBytes(max_size),
                nfiles = max_nfiles,
                s = if max_nfiles == 1 { "" } else { "s" },
            )
        }
    }
}
