use std::borrow::Cow;
use std::time::Duration;

use futures::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use tokio::process::ChildStdout;

use crate::borg::{self, Event, LogLevel, Progress};
use crate::child::{self, Child, Spawn};

#[derive(clap::Args)]
pub struct Args {
    /// Only perform repository checks (chunk CRCs)
    #[arg(long)]
    repository_only: bool,
}

pub async fn main(args: Args) -> child::Result<()> {
    let mut cmdline = vec!["borg", "check", "-v", "--progress", "--log-json"];
    if args.repository_only {
        cmdline.push("--repository-only");
    }
    let (spawn, output) = Child::from_cmdline(&cmdline).spawn_with_output()?;
    render(spawn, output).await
}

async fn render(mut spawn: Spawn, output: ChildStdout) -> child::Result<()> {
    let style = ProgressStyle::with_template("[boi] {spinner} {bar} {pos}/{len} • {wide_msg}")
        .expect("hardcoded ProgressStyle template should be valid");

    let new_waiting_spinner = || {
        let bar = ProgressBar::no_length();
        bar.set_style(style.clone());
        bar.enable_steady_tick(Duration::from_millis(100));
        bar.set_message("Waiting for Borg");
        bar
    };

    let mut bar = new_waiting_spinner();
    let mut warned_once = false;

    let mut warn_once = |bar: &mut ProgressBar, msg: &str| {
        if !warned_once {
            bar.suspend(|| speak!("⚑", "{msg}"));
        }
        warned_once = true;
    };

    let mut event_stream = borg::stream(output);
    while let Some(event) = event_stream.next().await {
        match event {
            Ok(Event::ProgressPercent(Progress::Running(progress))) => {
                bar.set_length(progress.total);
                bar.set_position(progress.current);
                bar.set_message(progress.message);
            }
            Ok(Event::ProgressPercent(Progress::Finished)) => {
                bar.finish_and_clear();
                bar = new_waiting_spinner();
            }
            Ok(Event::LogMessage(msg)) if msg.level >= LogLevel::Warning => {
                bar.suspend(|| speak!("⚑", "{}", msg.message));
            }
            Ok(Event::Unknown(event_type)) => {
                warn_once(
                    &mut bar,
                    &match event_type {
                        None => Cow::Borrowed("Unrecognized event from Borg"),
                        Some(ty) => Cow::Owned(format!("Unrecognized {ty} event from Borg")),
                    },
                );
            }
            Err(err) => {
                warn_once(
                    &mut bar,
                    &format!("Ignoring further Borg output due to JSON error: {err}"),
                );
                break;
            }
            _ => {}
        }
    }

    let child_result = match tokio::time::timeout(Duration::from_millis(500), spawn.wait()).await {
        Ok(result) => result,
        Err(_timeout) => {
            bar.set_message("Waiting for Borg to exit");
            spawn.wait().await
        }
    };

    bar.finish_and_clear();
    match &child_result {
        Ok(()) => {
            speak!("✓", "Repository is valid");
        }
        Err(child::Error::ExitCode(code)) => {
            speak!("✗", "Borg exited with code {code}");
        }
        Err(child::Error::Killed) => {
            speak!("✗", "Borg terminated abnormally");
        }
        Err(child::Error::Launch(err)) => {
            speak!("✗", "Failed to wait for Borg: {err}");
        }
    }

    child_result
}
