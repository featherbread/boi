use std::borrow::Cow;
use std::fmt::{self, Display};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use futures::StreamExt;
use indicatif::style::ProgressTracker;
use indicatif::{HumanBytes, ProgressBar, ProgressState, ProgressStyle};
use serde::de::IgnoredAny;
use serde_derive::Deserialize;
use serde_json::{json, Map as JsonMap, Value as JsonValue};
use tokio::process::ChildStdout;

use crate::child::Spawn;
use crate::json::JsonStream;

pub async fn render(mut spawn: Spawn, output: ChildStdout) {
    let last_stats = Arc::new(RwLock::new(ArchiveStats::default()));
    let bar = ProgressBar::new_spinner();
    bar.set_style(ArchiveStats::bar_style(&last_stats));
    bar.enable_steady_tick(Duration::from_millis(100));
    bar.set_message("Waiting for Borg to start");

    let mut warned_once = false;
    let mut warn_once = |msg: &str| {
        if !warned_once {
            bar.suspend(|| speak!("⚑", "{msg}"));
        }
        warned_once = true;
    };

    let mut final_stats = None;
    let mut output_stream = JsonStream::new(output);
    while let Some(raw_log) = output_stream.next().await {
        let raw_event = match raw_log {
            Ok(BorgJson::CreateEvent(raw_event)) => raw_event,
            Ok(BorgJson::FinalStats(stats)) => {
                final_stats = Some(stats);
                continue;
            }
            Ok(BorgJson::Unknown(_)) => {
                warn_once("Unrecognized log entry from Borg");
                continue;
            }
            Err(err) => {
                warn_once(&format!(
                    "Ignoring further Borg output due to JSON error: {err}"
                ));
                break;
            }
        };

        let bar_message = match CreateEvent::from(raw_event) {
            CreateEvent::Blank => continue,
            CreateEvent::ProgressMessage(msg) => Cow::Owned(msg),
            CreateEvent::ArchiveFinished => Cow::Borrowed("Finished archiving files"),
            CreateEvent::ArchiveProgress(progress) => {
                *last_stats.write().unwrap() = progress.stats;
                Cow::Owned(progress.path)
            }
            CreateEvent::LogMessage(msg) => {
                bar.suspend(|| speak!("⚑", "{msg}"));
                continue;
            }
            CreateEvent::Unrecognized(msg_type) => {
                warn_once(&format!("Unrecognized {msg_type} event from Borg"));
                continue;
            }
        };

        bar.set_message(bar_message);
    }

    let mut duration = None;
    if let Some(stats) = final_stats {
        duration = Some(stats.archive.duration);
        *last_stats.write().unwrap() = stats.archive.stats;
    }

    let child_result = match tokio::time::timeout(Duration::from_millis(500), spawn.wait()).await {
        Ok(result) => result,
        Err(_timeout) => {
            bar.set_message("Waiting for Borg to exit");
            spawn.wait().await
        }
    };

    match child_result {
        Ok(status) if status.success() => {
            // NOTE: Providing just one tick string introduces a panic risk.
            bar.set_style(ArchiveStats::bar_style(&last_stats).tick_strings(&["✓", "✓"]));
            if let Some(duration) = duration {
                bar.finish_with_message(format!("Created archive in {duration} seconds"));
            } else {
                bar.finish_with_message("Created archive");
            }
        }
        Ok(status) => {
            bar.set_style(ArchiveStats::bar_style(&last_stats).tick_strings(&["✗", "✗"]));
            if let Some(code) = status.code() {
                bar.finish_with_message(format!("Borg exited with code {code}"));
            } else {
                bar.finish_with_message(format!("Borg terminated abnormally: {status}"));
            }
        }
        Err(err) => {
            bar.set_style(ArchiveStats::bar_style(&last_stats).tick_strings(&["✗", "✗"]));
            bar.finish_with_message(format!("Failed to wait for Borg: {err}"));
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum BorgJson {
    CreateEvent(CreateJson),
    FinalStats(StatsJson),
    Unknown(IgnoredAny),
}

#[derive(Deserialize)]
struct StatsJson {
    archive: StatsArchiveJson,
}

#[derive(Deserialize)]
struct StatsArchiveJson {
    duration: f64,
    stats: ArchiveStats,
}

#[derive(Deserialize)]
struct CreateJson {
    #[serde(rename = "type")]
    msg_type: String,
    #[serde(flatten)]
    rest: JsonMap<String, JsonValue>,
}

impl CreateJson {
    fn message(&self) -> Option<String> {
        self.rest
            .get("message")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
    }
}

enum CreateEvent {
    Blank,
    ArchiveProgress(ArchiveProgress),
    ArchiveFinished,
    ProgressMessage(String),
    LogMessage(String),
    Unrecognized(String),
}

impl From<CreateJson> for CreateEvent {
    fn from(raw: CreateJson) -> Self {
        match raw.msg_type.as_str() {
            "archive_progress" if raw.rest.get("finished") == Some(&json!(true)) => {
                CreateEvent::ArchiveFinished
            }
            "archive_progress" => {
                serde_json::from_value::<ArchiveProgress>(JsonValue::Object(raw.rest))
                    .map(CreateEvent::ArchiveProgress)
                    .unwrap_or(CreateEvent::Unrecognized(raw.msg_type))
            }
            "progress_message" => raw
                .message()
                .map(CreateEvent::ProgressMessage)
                .unwrap_or(CreateEvent::Blank),

            "log_message" => raw
                .message()
                .map(CreateEvent::LogMessage)
                .unwrap_or(CreateEvent::Blank),

            _ => CreateEvent::Unrecognized(raw.msg_type),
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ArchiveProgress {
    path: String,
    #[serde(flatten)]
    stats: ArchiveStats,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ArchiveStats {
    nfiles: u64,
    original_size: u64,
    compressed_size: u64,
    deduplicated_size: u64,
}

impl ArchiveStats {
    fn bar_style(stats: &Arc<RwLock<ArchiveStats>>) -> ProgressStyle {
        ProgressStyle::with_template(
            "[boi] {spinner} {nfiles} N {orig} S {comp} C {ddup} D • {wide_msg}",
        )
        .expect("hardcoded ProgressStyle template should be valid")
        .with_key(
            "nfiles",
            ArchiveStatsTracker::new(stats, ArchiveStats::bar_nfiles),
        )
        .with_key(
            "orig",
            ArchiveStatsTracker::new(stats, ArchiveStats::bar_orig),
        )
        .with_key(
            "comp",
            ArchiveStatsTracker::new(stats, ArchiveStats::bar_comp),
        )
        .with_key(
            "ddup",
            ArchiveStatsTracker::new(stats, ArchiveStats::bar_ddup),
        )
    }

    fn bar_nfiles(&self) -> u64 {
        self.nfiles
    }

    fn bar_orig(&self) -> HumanBytes {
        HumanBytes(self.original_size)
    }

    fn bar_comp(&self) -> HumanBytes {
        HumanBytes(self.compressed_size)
    }

    fn bar_ddup(&self) -> HumanBytes {
        HumanBytes(self.deduplicated_size)
    }
}

#[derive(Clone)]
struct ArchiveStatsTracker<F> {
    stats: Arc<RwLock<ArchiveStats>>,
    render: F,
}

impl<F> ArchiveStatsTracker<F> {
    fn new(stats: &Arc<RwLock<ArchiveStats>>, render: F) -> Self {
        Self {
            stats: Arc::clone(stats),
            render,
        }
    }
}

impl<F, T> ProgressTracker for ArchiveStatsTracker<F>
where
    F: Fn(&ArchiveStats) -> T + Clone + Send + Sync + 'static,
    T: Display,
{
    fn clone_box(&self) -> Box<dyn ProgressTracker> {
        Box::new(self.clone())
    }

    fn tick(&mut self, _: &ProgressState, _: Instant) {}

    fn reset(&mut self, _: &ProgressState, _: Instant) {}

    fn write(&self, _: &ProgressState, w: &mut dyn fmt::Write) {
        let stat = (self.render)(&self.stats.read().unwrap());
        let _ = write!(w, "{}", stat);
    }
}
