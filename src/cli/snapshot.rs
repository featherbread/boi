use std::cmp;
use std::env;
use std::fmt::{self, Display};
use std::ops::ControlFlow;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime};

use futures::StreamExt;
use indicatif::HumanBytes;

use crate::borg::{self, ArchiveStats, Event, Progress};
use crate::child::{self, Child};
use crate::config::{Config, RepoConfig};
use crate::reporting::{RepoReporter, ReporterSet, Widget};

#[cfg(boi_has_driver = "apfs")]
use crate::snapshot::driver_apfs;
#[cfg(boi_has_driver = "none")]
use crate::snapshot::driver_none;

#[derive(clap::Args)]
pub struct Args {
    /// The repository to work on
    repository: Option<String>,

    /// How to snapshot the home directory
    #[arg(long)]
    #[arg(default_value_t)]
    driver: DriverKind,

    #[cfg(boi_has_driver = "apfs")]
    #[command(flatten)]
    apfs: driver_apfs::Args,
}

#[derive(Clone, clap::ValueEnum)]
pub enum DriverKind {
    #[cfg(boi_has_driver = "apfs")]
    Apfs,
    #[cfg(boi_has_driver = "none")]
    None,
}

#[expect(clippy::derivable_impls)] // False positive, clippy doesn't understand this construction.
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
    let repos = match &args.repository {
        Some(name) => vec![(name.as_str(), config.get_or_die(name))],
        None => config.repos().collect(),
    };

    let Ok(ts) = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) else {
        die!("System time is before the UNIX epoch; what are you doing?!?");
    };

    let run = async move {
        let all_stats: SharedArchiveStatsList = Default::default();
        let mut reporter_set = ReporterSet::new(Widget::new(ArchiveStatsSummaryRunning(
            Arc::clone(&all_stats),
        )));

        let tasks: Vec<_> = repos
            .into_iter()
            .map(|(name, repo)| Task::new(name.to_owned(), repo.clone(), ts))
            .map(|task| {
                let name = task.name().to_owned();
                let header = Widget::new(ArchiveStatsDisplay(Arc::clone(task.last_stats())));
                (task, reporter_set.add_repo(name, header))
            })
            .collect();

        all_stats
            .lock()
            .unwrap()
            .extend(tasks.iter().map(|(task, _)| Arc::clone(task.last_stats())));

        let spawns: Vec<_> = tasks
            .into_iter()
            .map(|(task, reporter)| tokio::spawn(task.run(reporter)))
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
                let stats = ArchiveStatsSummary(all_stats);
                reporter_set.finish("✓", format!("Archived {stats}"));
                Ok(())
            }
            Some(err) => {
                reporter_set.finish("✗", "Failed archiving to some repos");
                Err(err)
            }
        }
    };

    match args.driver {
        #[cfg(boi_has_driver = "apfs")]
        DriverKind::Apfs => driver_apfs::in_backup_root(args.apfs, run).await,
        #[cfg(boi_has_driver = "none")]
        DriverKind::None => driver_none::in_backup_root(run).await,
    }
}

struct Task {
    name: String,
    repo: RepoConfig,
    ts: Duration,
    last_stats: SharedArchiveStats,
}

impl Task {
    fn new(name: String, repo: RepoConfig, ts: Duration) -> Self {
        Self {
            name,
            repo,
            ts,
            last_stats: Arc::new(RwLock::new(ArchiveStats::default())),
        }
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn last_stats(&self) -> &SharedArchiveStats {
        &self.last_stats
    }

    async fn run(self, mut reporter: RepoReporter) -> child::Result<()> {
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
            .spawn_with_output()
            .await?;

        let mut archive_complete_event = None;
        let mut event_stream = borg::stream(output);
        while let Some(event) = event_stream.next().await {
            match event {
                Ok(Event::ProgressMessage(msg)) => {
                    reporter.post_message(msg);
                }
                Ok(Event::ArchiveProgress(Progress::Finished)) => {
                    reporter.post_message("Finished archiving files");
                }
                Ok(Event::ArchiveProgress(Progress::Running(progress))) => {
                    *self.last_stats.write().unwrap() = progress.stats;
                    reporter.post_message(progress.path);
                }
                Ok(Event::ArchiveComplete(event)) => {
                    archive_complete_event = Some(event);
                }
                Ok(Event::LogMessage(msg)) => {
                    reporter.suspend(|| speak!("⚑", "{}", msg.message));
                }
                event => match reporter.post_unhandled_event(event) {
                    ControlFlow::Continue(()) => {}
                    ControlFlow::Break(()) => break,
                },
            }
        }

        let mut duration = None;
        if let Some(event) = archive_complete_event {
            duration = Some(event.duration);
            *self.last_stats.write().unwrap() = event.stats;
        }

        let child_result = reporter
            .wait_for_spawn(&mut spawn, "Waiting for Borg to exit")
            .await;

        let (sigil, message) = match &child_result {
            Ok(()) => (
                "✓",
                format!(
                    "Created archive{suffix}",
                    suffix = duration
                        .map(|d| format!(" in {d} seconds"))
                        .unwrap_or_default(),
                ),
            ),
            Err(child::Error::Killed) => ("✗", "Borg terminated abnormally".to_owned()),
            Err(child::Error::ExitCode(code)) => ("✗", format!("Borg exited with code {code}")),
            Err(child::Error::Launch(err)) => ("✗", format!("Failed to wait for Borg: {err}")),
        };

        reporter.finish_once(sigil, message);
        child_result
    }
}

type SharedArchiveStats = Arc<RwLock<ArchiveStats>>;

type SharedArchiveStatsList = Arc<Mutex<Vec<SharedArchiveStats>>>;

pub struct ArchiveStatsDisplay(SharedArchiveStats);

impl Display for ArchiveStatsDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let stats = self.0.read().unwrap();
        write!(f, "{stats}")
    }
}

pub struct ArchiveStatsSummaryRunning(SharedArchiveStatsList);

impl Display for ArchiveStatsSummaryRunning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let summary = ArchiveStatsSummary(Arc::clone(&self.0));
        write!(f, "Archiving {summary}…")
    }
}

pub struct ArchiveStatsSummary(SharedArchiveStatsList);

impl Display for ArchiveStatsSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (max_nfiles, max_size) = self
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|s| {
                let s = s.read().unwrap();
                (s.nfiles, s.original_size)
            })
            .reduce(|(na, sa), (nb, sb)| (cmp::max(na, nb), cmp::max(sa, sb)))
            .unwrap_or_default();

        if max_nfiles == 0 && max_size == 0 {
            write!(f, "files")
        } else {
            write!(
                f,
                "{orig} in {nfiles} file{s}",
                orig = HumanBytes(max_size),
                nfiles = max_nfiles,
                s = if max_nfiles == 1 { "" } else { "s" },
            )
        }
    }
}
