use std::ops::ControlFlow;

use futures::StreamExt;

use crate::borg::{self, Event, LogLevel, Progress};
use crate::child::{self, Child};
use crate::config::{Config, RepoConfig};
use crate::reporting::{RepoReporter, ReporterSet, Widget};

#[derive(clap::Args)]
pub struct Args {
    /// Specific repositories to check
    repositories: Vec<String>,

    /// Only perform repository checks (chunk CRCs)
    #[arg(long)]
    repository_only: bool,
}

pub async fn main(args: Args) -> child::Result<()> {
    let config = Config::load_or_die().await;
    let repos: Vec<_> = if args.repositories.is_empty() {
        config.repos().collect()
    } else {
        config.select_repos_or_die(&args.repositories).collect()
    };

    let mut reporter_set = ReporterSet::new(Widget::from_message("Checking repositories…"));

    let spawns: Vec<_> = repos
        .into_iter()
        .map(|(name, repo)| {
            let reporter = reporter_set.add_repo(name.to_owned(), Widget::from_message(""));
            tokio::spawn(run(repo.clone(), reporter, args.repository_only))
        })
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
            reporter_set.finish("✓", "Confirmed all repositories are valid.");
            Ok(())
        }
        Some(err) => {
            reporter_set.finish("✗", "Found issues in repositories!");
            Err(err)
        }
    }
}

async fn run(
    repo: RepoConfig,
    mut reporter: RepoReporter,
    repository_only: bool,
) -> child::Result<()> {
    let mut cmdline = vec!["borg", "check", "-v", "--progress", "--log-json"];
    if repository_only {
        cmdline.push("--repository-only");
    }

    let (mut spawn, output) = Child::from_cmdline(&cmdline)
        .for_borg_repo(&repo)
        .spawn_with_output()
        .await?;

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
        .wait_for_spawn(&mut spawn, "Waiting for Borg to exit…")
        .await;

    let (sigil, message) = match &child_result {
        Ok(()) => ("✓", "Repository is valid".to_owned()),
        Err(child::Error::ExitCode(code)) => ("✗", format!("Borg exited with code {code}")),
        Err(child::Error::Killed) => ("✗", "Borg terminated abnormally".to_owned()),
        Err(child::Error::Launch(err)) => ("✗", format!("Failed to wait for Borg: {err}")),
    };

    reporter.finish_once(sigil, message);
    child_result
}
