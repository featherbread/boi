use std::borrow::Cow;
use std::fmt::{self, Display};
use std::io::Write;
use std::mem::{self, Discriminant};
use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::{self, Duration};

use console::Term;
use indicatif::style::ProgressTracker;
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressState, ProgressStyle};

use crate::borg::{Event, ProgressPercent};
use crate::child;

const TICK_INTERVAL: Duration = Duration::from_millis(100);

pub struct ReporterBuilder {
    header: Widget,
    repos: Vec<(String, Widget)>,
}

impl ReporterBuilder {
    pub fn new(header: Widget) -> Self {
        Self {
            header,
            repos: Vec::new(),
        }
    }

    pub fn register_repo(&mut self, name: String, header: Widget) {
        self.repos.push((name, header));
    }

    pub fn finish(self) -> ReporterSet {
        let mp = MultiProgress::new();

        let head_bar = mp.add(ProgressBar::no_length());
        head_bar.enable_steady_tick(TICK_INTERVAL);
        let head = HeadReporter::new_with_bar(head_bar, self.header);

        let repos = self
            .repos
            .into_iter()
            .map(|(name, header)| {
                let bar = mp.add(ProgressBar::no_length());
                bar.enable_steady_tick(TICK_INTERVAL);
                let mut repo = RepoReporter {
                    name,
                    sigil: None,
                    header,
                    report: Report::Message("".into()),
                    mp: mp.clone(),
                    bar,
                    current_style: None,
                    did_once: false,
                };
                repo.post_message("Starting up…");
                repo
            })
            .collect();

        ReporterSet { head, repos, mp }
    }
}

pub struct ReporterSet {
    head: HeadReporter,
    repos: Vec<RepoReporter>,

    mp: MultiProgress,
}

impl ReporterSet {
    pub fn repos(&mut self) -> Vec<&mut RepoReporter> {
        self.repos.iter_mut().collect()
    }

    pub fn finish(mut self, sigil: &'static str, msg: impl Into<Cow<'static, str>>) {
        self.head.finish(sigil, msg);
        for mut repo in self.repos {
            if !repo.is_finished() {
                repo.finish("⚠", "Final status unknown.");
            }
        }

        let _ = self.mp.clear();
        let (width, height) = Term::stdout().size();
        let target = indicatif::InMemoryTerm::new(width, height);
        self.mp
            .set_draw_target(ProgressDrawTarget::term_like(Box::new(target.clone())));
        self.mp.suspend(|| {});

        let _ = Term::stdout().write_all(target.contents().as_bytes());
        let _ = Term::stdout().write_all(b"\n");
    }
}

struct HeadReporter {
    sigil: Option<&'static str>,
    header: Widget,

    bar: ProgressBar,
}

impl HeadReporter {
    fn new_with_bar(bar: ProgressBar, header: Widget) -> Self {
        let mut head = Self {
            sigil: None,
            header,
            bar,
        };
        head.refresh_bar();
        head
    }

    fn finish(&mut self, sigil: &'static str, msg: impl Into<Cow<'static, str>>) {
        self.sigil = Some(sigil);
        self.header = Widget::from_message(msg);
        self.refresh_bar();
        self.bar.finish();
    }

    fn refresh_bar(&mut self) {
        self.bar.set_style(self.create_style());
    }

    fn create_style(&self) -> ProgressStyle {
        match self.sigil {
            None => ProgressStyle::with_template("[boi] {spinner} {header}")
                .expect("hardcoded ProgressStyle template should be valid")
                .with_key("header", self.header.clone()),
            Some(sigil) => ProgressStyle::with_template("[boi] {sigil} {header}")
                .expect("hardcoded ProgressStyle template should be valid")
                .with_key("header", self.header.clone())
                .with_key("sigil", Widget::from_message(sigil)),
        }
    }
}

const DEFAULT_REPO_SIGIL: &str = "─";

pub struct RepoReporter {
    name: String,
    sigil: Option<&'static str>,
    header: Widget,
    report: Report,

    mp: MultiProgress,
    bar: ProgressBar,
    current_style: Option<Discriminant<Report>>,
    did_once: bool,
}

enum Report {
    Message(Cow<'static, str>),
    Progress(ProgressPercent),
}

impl RepoReporter {
    pub fn post_message(&mut self, msg: impl Into<Cow<'static, str>>) {
        self.report = Report::Message(msg.into());
        self.refresh_bar();
    }

    pub fn post_progress(&mut self, progress: ProgressPercent) {
        self.report = Report::Progress(progress);
        self.refresh_bar();
    }

    pub fn suspend(&mut self, f: impl FnOnce()) {
        self.mp.suspend(f);
    }

    pub fn suspend_once(&mut self, f: impl FnOnce()) {
        if !self.did_once {
            self.mp.suspend(f);
            self.did_once = true;
        }
    }

    pub fn post_unhandled_event(&mut self, event: serde_json::Result<Event>) -> ControlFlow<()> {
        match event {
            Ok(Event::Unknown(None)) => {
                self.suspend_once(|| speak!("⚑", "Unrecognized event from Borg"));
                ControlFlow::Continue(())
            }
            Ok(Event::Unknown(Some(ty))) => {
                self.suspend_once(|| speak!("⚑", "Unrecognized {ty} event from Borg"));
                ControlFlow::Continue(())
            }
            Err(err) => {
                self.suspend(|| {
                    speak!("⚑", "Ignoring further Borg output due to JSON error: {err}")
                });
                ControlFlow::Break(())
            }
            _ => ControlFlow::Continue(()),
        }
    }

    pub async fn wait_for_spawn(
        &mut self,
        spawn: &mut child::Spawn,
        msg: impl Into<Cow<'static, str>>,
    ) -> child::Result<()> {
        match tokio::time::timeout(Duration::from_millis(500), spawn.wait()).await {
            Ok(result) => result,
            Err(_timeout) => {
                self.post_message(msg);
                spawn.wait().await
            }
        }
    }

    pub fn finish(&mut self, sigil: &'static str, msg: impl Into<Cow<'static, str>>) {
        self.sigil = Some(sigil);
        self.current_style = None; // Force a restyle of the bar.
        self.post_message(msg);
        self.bar.finish();
    }

    fn is_finished(&self) -> bool {
        self.sigil.is_some()
    }

    fn refresh_bar(&mut self) {
        let want_style = mem::discriminant(&self.report);
        if self.current_style != Some(want_style) {
            self.switch_bar_style();
            self.current_style = Some(want_style);
        }

        match &self.report {
            Report::Message(msg) => {
                self.bar.set_message(msg.clone());
            }
            Report::Progress(progress) => {
                self.bar.set_message(progress.message.clone());
                self.bar.set_length(progress.total);
                self.bar.set_position(progress.current);
            }
        }
    }

    fn switch_bar_style(&mut self) {
        self.bar.set_style(match &self.report {
            Report::Message(_) => self.create_message_style(),
            Report::Progress(_) => self.create_progress_style(),
        })
    }

    fn create_message_style(&self) -> ProgressStyle {
        self.create_style(&[
            "      ┌ {name} {sigil} {header}", // rustfmt keep line break
            "      └ {wide_msg}",              // rustfmt keep line break
        ])
        .with_key(
            "sigil",
            if let Some(sigil) = self.sigil {
                Widget::from_message(sigil)
            } else if self.header.to_string().is_empty() {
                Widget::from_message("")
            } else {
                Widget::from_message(DEFAULT_REPO_SIGIL)
            },
        )
    }

    fn create_progress_style(&self) -> ProgressStyle {
        self.create_style(&[
            "      ┌ {name} {sigil} {wide_msg}", // rustfmt keep line break
            "      └ {bar} {pos}/{len}",         // rustfmt keep line break
        ])
        .with_key(
            "sigil",
            Widget::from_message(self.sigil.unwrap_or(DEFAULT_REPO_SIGIL)),
        )
    }

    fn create_style(&self, template: &[&'static str]) -> ProgressStyle {
        ProgressStyle::with_template(&template.join("\n"))
            .expect("hardcoded ProgressStyle template should be valid")
            .with_key("name", Widget::from_message(self.name.clone()))
            .with_key("header", self.header.clone())
    }
}

#[derive(Clone)]
pub struct Widget(Arc<dyn Display + Send + Sync + 'static>);

impl Widget {
    pub fn new(inner: impl Display + Send + Sync + 'static) -> Self {
        Self(Arc::new(inner))
    }

    pub fn from_message(msg: impl Into<Cow<'static, str>>) -> Self {
        Self::new(msg.into())
    }
}

impl Display for Widget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl ProgressTracker for Widget {
    fn clone_box(&self) -> Box<dyn ProgressTracker> {
        Box::new(Widget(Arc::clone(&self.0)))
    }

    fn tick(&mut self, _: &ProgressState, _: time::Instant) {}

    fn reset(&mut self, _: &ProgressState, _: time::Instant) {}

    fn write(&self, _: &ProgressState, w: &mut dyn fmt::Write) {
        let _ = write!(w, "{}", self.0);
    }
}
