use std::borrow::Cow;
use std::mem::{self, Discriminant};
use std::sync::LazyLock;
use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};

use crate::borg::ProgressPercent;
use crate::child;

pub struct Reporter {
    bar: ProgressBar,
    style: StyleMode,
    did_once: bool,
}

enum Report {
    Message(Cow<'static, str>),
    Progress(ProgressPercent),
}

enum StyleMode {
    Auto(Discriminant<Report>),
    Forced,
}

impl Reporter {
    pub fn new<S>(msg: S) -> Self
    where
        S: Into<Cow<'static, str>>,
    {
        let state = Report::Message(msg.into());
        let mut reporter = Self {
            bar: ProgressBar::no_length(),
            style: StyleMode::Auto(mem::discriminant(&state)),
            did_once: false,
        };

        switch_bar_style(&mut reporter.bar, &state);
        reporter.bar.enable_steady_tick(Duration::from_millis(100));
        reporter.post(state);

        reporter
    }

    pub fn force_style(&mut self, style: ProgressStyle) {
        self.bar.set_style(style);
        self.style = StyleMode::Forced;
    }

    pub fn post_message<S>(&mut self, msg: S)
    where
        S: Into<Cow<'static, str>>,
    {
        self.post(Report::Message(msg.into()));
    }

    pub fn post_progress(&mut self, progress: ProgressPercent) {
        self.post(Report::Progress(progress));
    }

    fn post(&mut self, state: Report) {
        let want_style = mem::discriminant(&state);
        if let StyleMode::Auto(have_style) = self.style
            && have_style != want_style
        {
            switch_bar_style(&mut self.bar, &state);
            self.style = StyleMode::Auto(want_style);
        }

        match state {
            Report::Message(msg) => {
                self.bar.set_message(msg);
            }
            Report::Progress(progress) => {
                self.bar.set_message(progress.message);
                self.bar.set_length(progress.total);
                self.bar.set_position(progress.current);
            }
        }
    }

    pub fn suspend<F>(&mut self, op: F)
    where
        F: FnOnce(),
    {
        self.bar.suspend(op);
    }

    pub fn suspend_once<F>(&mut self, op: F)
    where
        F: FnOnce(),
    {
        if !self.did_once {
            self.suspend(op);
            self.did_once = true;
        }
    }

    pub async fn wait_for_spawn<S>(&mut self, spawn: &mut child::Spawn, msg: S) -> child::Result<()>
    where
        S: Into<Cow<'static, str>>,
    {
        match tokio::time::timeout(Duration::from_millis(500), spawn.wait()).await {
            Ok(result) => result,
            Err(_timeout) => {
                self.post_message(msg);
                spawn.wait().await
            }
        }
    }

    pub fn clear(self) {
        self.bar.finish_and_clear();
    }
}

fn switch_bar_style(bar: &mut ProgressBar, state: &Report) {
    match state {
        Report::Message(_) => bar.set_style(message_style()),
        Report::Progress(_) => bar.set_style(progress_style()),
    }
}

fn message_style() -> ProgressStyle {
    static STYLE: LazyLock<ProgressStyle> = LazyLock::new(|| {
        ProgressStyle::with_template("[boi] {spinner} {wide_msg}")
            .expect("hardcoded ProgressStyle template should be valid")
    });
    STYLE.clone()
}

fn progress_style() -> ProgressStyle {
    static STYLE: LazyLock<ProgressStyle> = LazyLock::new(|| {
        ProgressStyle::with_template("[boi] {spinner} {bar} {pos}/{len} • {wide_msg}")
            .expect("hardcoded ProgressStyle template should be valid")
    });
    STYLE.clone()
}
