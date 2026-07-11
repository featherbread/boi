use std::borrow::Cow;
use std::fmt::{self, Display};
use std::io::Write;
use std::marker::PhantomData;
use std::mem::{self, Discriminant};
use std::ops::ControlFlow;
use std::sync::{Arc, RwLock};
use std::time::{self, Duration};

use console::Term;
use indicatif::style::ProgressTracker;
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressState, ProgressStyle};

use crate::borg::{Event, ProgressPercent};
use crate::child;

const TICK_INTERVAL: Duration = Duration::from_millis(100);

#[repr(transparent)]
pub struct ReporterSet<K: reporterset::Kind>(ReporterSetState, PhantomData<K>);

struct ReporterSetState {
    mp: MultiProgress,
    head: HeadReporter,
    repos: Vec<RepoReporter>,
}

pub enum ReposAddable {}
pub enum ReposLocked {}

mod reporterset {
    pub trait Kind {}
    impl Kind for super::ReposAddable {}
    impl Kind for super::ReposLocked {}
}

impl ReporterSet<ReposAddable> {
    pub fn new(header: Widget) -> Self {
        let mp = MultiProgress::new();
        let head_bar = Self::new_bar_in(&mp);
        Self(
            ReporterSetState {
                mp,
                head: HeadReporter::new_with_bar(head_bar, header),
                repos: Vec::new(),
            },
            PhantomData,
        )
    }

    pub fn add_repo(&mut self, name: String, header: Widget) -> RepoReporter {
        let mut repo = RepoReporter(Arc::new(RwLock::new(RepoReporterState {
            mp: self.0.mp.clone(),
            bar: Self::new_bar_in(&self.0.mp),
            name,
            sigil: None,
            header,
            report: Report::Message("".into()),
            did_once: false,
            current_style: None,
        })));
        repo.post_message("Starting up…");
        repo
    }

    /// Enables features that reduce flickering of the output but require the number of displayed
    /// progress indicators to remain constant.
    pub fn lock_repos(self) -> ReporterSet<ReposLocked> {
        self.0.mp.set_move_cursor(true);
        ReporterSet(self.0, PhantomData)
    }

    fn new_bar_in(mp: &MultiProgress) -> ProgressBar {
        let bar = mp.add(ProgressBar::no_length());
        bar.enable_steady_tick(TICK_INTERVAL);
        bar
    }
}

impl<K: reporterset::Kind> ReporterSet<K> {
    pub fn finish(mut self, sigil: &'static str, msg: impl Into<Cow<'static, str>>) {
        self.0.head.finish(sigil, msg);
        for mut repo in self.0.repos {
            repo.finish_once("⚠", "Final status unknown.");
        }

        // As of writing, indicatif finishes non-cleared bars with padding between the right edge
        // of the progress output and the edge of the terminal, which causes odd rewrapping when
        // the terminal is later resized. It may not be feasible to deal with resizing while the
        // bars are _running_, but it really bugs me when this happens even after boi has exited.
        // Since I don't want to duplicate the formatting from the ProgressStyle template strings,
        // I have indicatif render to an in-memory fake terminal of the same size as the current
        // stdout, and dump out the resulting contents, which don't include that same padding.
        let mut stdout = Term::stdout();
        let (width, height) = stdout.size();
        let target = indicatif::InMemoryTerm::new(width, height);
        let _ = self.0.mp.clear();
        self.0
            .mp
            .set_draw_target(ProgressDrawTarget::term_like(Box::new(target.clone())));

        // This seems to reliably force a draw to the in-memory terminal, in lieu of an actual
        // MultiProgress::force_draw.
        self.0.mp.suspend(|| {});

        let _ = stdout.write_all(target.contents().as_bytes());
        let _ = stdout.write_all(b"\n");
    }
}

struct HeadReporter {
    bar: ProgressBar,
    sigil: Option<&'static str>,
    header: Widget,
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
        self.header = Widget::text(msg);
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
                .with_key("sigil", Widget::text(sigil)),
        }
    }
}

const DEFAULT_REPO_SIGIL: &str = "─";

#[derive(Clone)]
pub struct RepoReporter(Arc<RwLock<RepoReporterState>>);

pub struct RepoReporterState {
    mp: MultiProgress,
    bar: ProgressBar,

    name: String,
    sigil: Option<&'static str>,
    header: Widget,
    report: Report,

    did_once: bool,
    current_style: Option<Discriminant<Report>>,
}

enum Report {
    Message(Cow<'static, str>),
    Progress(ProgressPercent),
}

impl RepoReporter {
    pub fn post_message(&mut self, msg: impl Into<Cow<'static, str>>) {
        let mut me = self.0.write().unwrap();
        me.report = Report::Message(msg.into());
        me.refresh_bar();
    }

    pub fn post_progress(&mut self, progress: ProgressPercent) {
        let mut me = self.0.write().unwrap();
        me.report = Report::Progress(progress);
        me.refresh_bar();
    }

    pub fn suspend(&mut self, f: impl FnOnce()) {
        let me = self.0.read().unwrap();
        me.mp.suspend(f);
    }

    pub fn suspend_once(&mut self, f: impl FnOnce()) {
        let mut me = self.0.write().unwrap();
        if !me.did_once {
            me.mp.suspend(f);
            me.did_once = true;
        }
    }

    pub fn post_unhandled_event(&mut self, event: serde_json::Result<Event>) -> ControlFlow<()> {
        match event {
            Ok(Event::Unknown(None)) => {
                self.suspend_once(|| speak!("⚑", "Saw an unrecognized event from Borg."));
                ControlFlow::Continue(())
            }
            Ok(Event::Unknown(Some(ty))) => {
                self.suspend_once(|| speak!("⚑", "Saw an unrecognized {ty} event from Borg."));
                ControlFlow::Continue(())
            }
            Err(err) => {
                self.suspend(|| {
                    speak!(
                        "⚑",
                        "Saw invalid JSON from Borg ({err}); ignoring further output."
                    );
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

    pub fn finish_once(&mut self, sigil: &'static str, msg: impl Into<Cow<'static, str>>) {
        let mut me = self.0.write().unwrap();
        if me.sigil.is_none() {
            me.sigil = Some(sigil);
            me.report = Report::Message(msg.into());
            me.current_style = None; // Force a restyle of the bar.
            me.refresh_bar();
            me.bar.finish();
        }
    }
}

impl RepoReporterState {
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
                Widget::text(sigil)
            } else if self.header.is_empty() {
                Widget::blank()
            } else {
                Widget::text(DEFAULT_REPO_SIGIL)
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
            Widget::text(self.sigil.unwrap_or(DEFAULT_REPO_SIGIL)),
        )
    }

    fn create_style(&self, template: &[&'static str]) -> ProgressStyle {
        ProgressStyle::with_template(&template.join("\n"))
            .expect("hardcoded ProgressStyle template should be valid")
            .with_key("name", Widget::text(self.name.clone()))
            .with_key("header", self.header.clone())
    }
}

#[derive(Clone)]
pub struct Widget(Option<Arc<dyn Display + Send + Sync + 'static>>);

impl Widget {
    pub fn new(inner: impl Display + Send + Sync + 'static) -> Self {
        Self(Some(Arc::new(inner)))
    }

    pub fn text(msg: impl Into<Cow<'static, str>>) -> Self {
        Self::new(msg.into())
    }

    pub fn blank() -> Self {
        Self(None)
    }

    fn is_empty(&self) -> bool {
        match &self.0 {
            None => true,
            Some(me) => me.to_string().is_empty(),
        }
    }
}

impl Display for Widget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0
            .as_ref()
            .map(|me| write!(f, "{me}"))
            .unwrap_or(Ok(()))
    }
}

impl ProgressTracker for Widget {
    fn clone_box(&self) -> Box<dyn ProgressTracker> {
        Box::new(Widget(self.0.clone()))
    }

    fn tick(&mut self, _: &ProgressState, _: time::Instant) {}

    fn reset(&mut self, _: &ProgressState, _: time::Instant) {}

    fn write(&self, _: &ProgressState, w: &mut dyn fmt::Write) {
        if let Some(me) = &self.0 {
            let _ = write!(w, "{me}");
        }
    }
}
