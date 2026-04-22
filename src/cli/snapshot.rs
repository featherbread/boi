use std::env;
use std::fmt;
use std::fmt::Display;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use futures::StreamExt;
use indicatif::{ProgressState, ProgressStyle};
use tokio::process::ChildStdout;

use crate::borg::{self, ArchiveStats, Event, Progress};
use crate::child::{self, Child, Spawn};
use crate::reporting::Reporter;

#[cfg(boi_has_driver = "apfs")]
use crate::snapshot::driver_apfs;
#[cfg(boi_has_driver = "none")]
use crate::snapshot::driver_none;

#[derive(clap::Args)]
pub struct Args {
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
    let Ok(repo) = env::var("BORG_REPO")
        .map_err(|err| die!("Can't read $BORG_REPO ({err}); how can I back up?"));

    let Ok(ts) = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) else {
        die!("System time is before the UNIX epoch; what are you doing?!?");
    };

    let run = async {
        let backup_spec = format!("{repo}::{sec}", sec = ts.as_secs());
        let child = Child::from_cmdline(&[
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
        ]);

        let (spawn, output) = child.spawn_with_output()?;
        render(spawn, output).await
    };

    let result: child::Result<()> = match args.driver {
        #[cfg(boi_has_driver = "apfs")]
        DriverKind::Apfs => driver_apfs::in_backup_root(args.apfs, run).await,
        #[cfg(boi_has_driver = "none")]
        DriverKind::None => driver_none::in_backup_root(run).await,
    };
    if result.is_err() {
        die!("Borg did not succeed; you should look at that.");
    }
    Ok(())
}

pub async fn render(mut spawn: Spawn, output: ChildStdout) -> child::Result<()> {
    let last_stats = Arc::new(RwLock::new(ArchiveStats::default()));

    let mut reporter = Reporter::new("Waiting for Borg to start");
    reporter.force_style({
        let stats = Arc::clone(&last_stats);
        ProgressStyle::with_template("[boi] {spinner} {stats} • {wide_msg}")
            .expect("hardcoded ProgressStyle template should be valid")
            .with_key("stats", move |_: &ProgressState, w: &mut dyn fmt::Write| {
                let _ = write!(w, "{}", stats.read().unwrap());
            })
    });

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
                *last_stats.write().unwrap() = progress.stats;
                reporter.post_message(progress.path);
            }
            Ok(Event::ArchiveComplete(event)) => {
                archive_complete_event = Some(event);
            }
            Ok(Event::LogMessage(msg)) => {
                reporter.suspend(|| speak!("⚑", "{}", msg.message));
            }
            Ok(Event::Unknown(None)) => {
                reporter.suspend_once(|| speak!("⚑", "Unrecognized event from Borg"));
            }
            Ok(Event::Unknown(Some(ty))) => {
                reporter.suspend_once(|| speak!("⚑", "Unrecognized {ty} event from Borg"));
            }
            Err(err) => {
                reporter.suspend(|| {
                    speak!("⚑", "Ignoring further Borg output due to JSON error: {err}")
                });
                break;
            }
            _ => {}
        }
    }

    let mut duration = None;
    if let Some(event) = archive_complete_event {
        duration = Some(event.duration);
        *last_stats.write().unwrap() = event.stats;
    }

    let child_result = reporter
        .wait_for_spawn(&mut spawn, "Waiting for Borg to exit")
        .await;

    reporter.clear();
    let stats = last_stats.read().unwrap();
    match &child_result {
        Ok(()) => {
            if let Some(duration) = duration {
                speak!("✓", "{stats} • Created archive in {duration} seconds");
            } else {
                speak!("✓", "{stats} • Created archive");
            }
        }
        Err(child::Error::ExitCode(code)) => {
            speak!("✗", "{stats} • Borg exited with code {code}");
        }
        Err(child::Error::Killed) => {
            speak!("✗", "{stats} • Borg terminated abnormally");
        }
        Err(child::Error::Launch(err)) => {
            speak!("✗", "{stats} • Failed to wait for Borg: {err}");
        }
    }

    child_result
}
