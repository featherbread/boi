use std::borrow::Cow;
use std::mem::{self, Discriminant};
use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};

use crate::borg::ProgressPercent;

pub struct Reporter {
    bar: ProgressBar,
    last_style: Discriminant<Report>,
    did_once: bool,
}

pub enum Report {
    Message(Cow<'static, str>),
    Progress(ProgressPercent),
}

impl Reporter {
    pub fn new(state: Report) -> Self {
        let mut reporter = Self {
            bar: ProgressBar::no_length(),
            last_style: mem::discriminant(&state),
            did_once: false,
        };

        switch_bar_style(&mut reporter.bar, &state);
        reporter.bar.enable_steady_tick(Duration::from_millis(100));
        reporter.post(state);

        reporter
    }

    pub fn post(&mut self, state: Report) {
        let new_discriminant = mem::discriminant(&state);
        if self.last_style != new_discriminant {
            switch_bar_style(&mut self.bar, &state);
            self.last_style = new_discriminant;
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
    ProgressStyle::with_template("[boi] {spinner} {wide_msg}")
        .expect("hardcoded ProgressStyle template should be valid")
}

fn progress_style() -> ProgressStyle {
    ProgressStyle::with_template("[boi] {spinner} {bar} {pos}/{len} • {wide_msg}")
        .expect("hardcoded ProgressStyle template should be valid")
}
