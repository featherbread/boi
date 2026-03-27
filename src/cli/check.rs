use std::borrow::Cow;
use std::time::Duration;

use futures::StreamExt;
use tokio::process::ChildStdout;

use crate::borg::{self, Event, LogLevel, Progress};
use crate::child::{self, Child, Spawn};
use crate::progress::{Report, Reporter};

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
    let waiting_msg = Cow::Borrowed("Waiting for Borg");
    let mut reporter = Reporter::new(Report::Message(waiting_msg.clone()));

    let mut event_stream = borg::stream(output);
    while let Some(event) = event_stream.next().await {
        match event {
            Ok(Event::ProgressPercent(Progress::Running(progress))) => {
                reporter.post(Report::Progress(progress));
            }
            Ok(Event::ProgressPercent(Progress::Finished)) => {
                reporter.post(Report::Message(waiting_msg.clone()));
            }
            Ok(Event::LogMessage(msg)) if msg.level >= LogLevel::Warning => {
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

    let child_result = match tokio::time::timeout(Duration::from_millis(500), spawn.wait()).await {
        Ok(result) => result,
        Err(_timeout) => {
            reporter.post(Report::Message(waiting_msg));
            spawn.wait().await
        }
    };

    reporter.clear();
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
