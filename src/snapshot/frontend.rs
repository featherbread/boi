use std::borrow::Cow;
use std::fmt;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use futures::StreamExt;
use indicatif::{ProgressBar, ProgressState, ProgressStyle};
use tokio::process::ChildStdout;

use crate::borg::{self, ArchiveStats, Event, Progress};
use crate::child::{self, Spawn};

pub async fn render(mut spawn: Spawn, output: ChildStdout) -> child::Result<()> {
    let last_stats = Arc::new(RwLock::new(ArchiveStats::default()));

    let bar = ProgressBar::new_spinner();
    bar.set_style({
        let stats = Arc::clone(&last_stats);
        ProgressStyle::with_template("[boi] {spinner} {stats} • {wide_msg}")
            .expect("hardcoded ProgressStyle template should be valid")
            .with_key("stats", move |_: &ProgressState, w: &mut dyn fmt::Write| {
                let _ = write!(w, "{}", stats.read().unwrap());
            })
    });

    bar.enable_steady_tick(Duration::from_millis(100));
    bar.set_message("Waiting for Borg to start");

    let mut warned_once = false;
    let mut warn_once = |msg: &str| {
        if !warned_once {
            bar.suspend(|| speak!("⚑", "{msg}"));
        }
        warned_once = true;
    };

    let mut archive_complete_event = None;
    let mut event_stream = borg::stream(output);
    while let Some(event) = event_stream.next().await {
        match event {
            Ok(Event::ProgressMessage(msg)) => {
                bar.set_message(msg);
            }
            Ok(Event::ArchiveProgress(Progress::Finished)) => {
                bar.set_message("Finished archiving files");
            }
            Ok(Event::ArchiveProgress(Progress::Running(progress))) => {
                *last_stats.write().unwrap() = progress.stats;
                bar.set_message(progress.path);
            }
            Ok(Event::ArchiveComplete(event)) => {
                archive_complete_event = Some(event);
            }
            Ok(Event::LogMessage(msg)) => {
                bar.suspend(|| speak!("⚑", "{}", msg.message));
            }
            Ok(Event::Unknown(event_type)) => {
                warn_once(&match event_type {
                    None => Cow::Borrowed("Unrecognized event from Borg"),
                    Some(ty) => Cow::Owned(format!("Unrecognized {ty} event from Borg")),
                });
            }
            Err(err) => {
                warn_once(&format!(
                    "Ignoring further Borg output due to JSON error: {err}"
                ));
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

    let child_result = match tokio::time::timeout(Duration::from_millis(500), spawn.wait()).await {
        Ok(result) => result,
        Err(_timeout) => {
            bar.set_message("Waiting for Borg to exit");
            spawn.wait().await
        }
    };

    bar.finish_and_clear();
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
