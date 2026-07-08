use std::ops::ControlFlow;

use futures::StreamExt;
use tokio::process::ChildStdout;

use crate::borg::{self, Event, LogLevel, Progress};
use crate::child::{self, Child, Spawn};
use crate::config::Config;
use crate::reporting::{ReporterBuilder, Widget};

#[derive(clap::Args)]
pub struct Args {
    /// The repository to work on
    repository: Option<String>,

    /// Only perform repository checks (chunk CRCs)
    #[arg(long)]
    repository_only: bool,
}

pub async fn main(args: Args) -> child::Result<()> {
    let config = Config::load_or_die().await;
    let (name, repo) = match &args.repository {
        Some(name) => (name.as_str(), config.get_or_die(name)),
        None => config.one_or_die(),
    };

    let mut cmdline = vec!["borg", "check", "-v", "--progress", "--log-json"];
    if args.repository_only {
        cmdline.push("--repository-only");
    }
    let (spawn, output) = Child::from_cmdline(&cmdline)
        .for_borg_repo(repo)
        .spawn_with_output()
        .await?;

    render(name, spawn, output).await
}

async fn render(name: &str, mut spawn: Spawn, output: ChildStdout) -> child::Result<()> {
    let mut builder = ReporterBuilder::new(Widget::from_message("Checking repositories…"));

    builder.register_repo(name.to_owned(), Widget::from_message(""));

    let mut reporter_set = builder.finish();
    let mut reporter_repos = reporter_set.repos();
    let reporter = reporter_repos.get_mut(0).unwrap();

    let mut event_stream = borg::stream(output);
    while let Some(event) = event_stream.next().await {
        match event {
            Ok(Event::ProgressPercent(Progress::Running(progress))) => {
                reporter.post_progress(progress);
            }
            Ok(Event::ProgressPercent(Progress::Finished)) => {
                reporter.post_message("Waiting for Borg");
            }
            Ok(Event::LogMessage(msg)) if msg.level >= LogLevel::Warning => {
                reporter.suspend(|| speak!("⚑", "{}", msg.message));
            }
            event => match reporter.post_unhandled_event(event) {
                ControlFlow::Continue(()) => {}
                ControlFlow::Break(()) => break,
            },
        }
    }

    let child_result = reporter
        .wait_for_spawn(&mut spawn, "Waiting for Borg to exit")
        .await;

    let summary = match &child_result {
        Ok(_) => "Confirmed all repositories are healthy",
        Err(_) => "Found issues in repositories",
    };
    let (sigil, message) = match &child_result {
        Ok(()) => ("✓", "Repository is valid".to_owned()),
        Err(child::Error::ExitCode(code)) => ("✗", format!("Borg exited with code {code}")),
        Err(child::Error::Killed) => ("✗", "Borg terminated abnormally".to_owned()),
        Err(child::Error::Launch(err)) => ("✗", format!("Failed to wait for Borg: {err}")),
    };

    reporter.finish(sigil, message);
    reporter_set.finish(sigil, summary);

    child_result
}
