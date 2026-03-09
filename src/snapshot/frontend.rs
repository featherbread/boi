use std::borrow::Cow;
use std::fmt::{self, Display};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use futures::StreamExt;
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
    bar.set_style(ArchiveStats::bar_style(Arc::clone(&last_stats)));
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

    bar.finish_and_clear();
    let stats = last_stats.read().unwrap();
    match child_result {
        Ok(status) if status.success() => {
            if let Some(duration) = duration {
                speak!("✓", "{stats} • Created archive in {duration} seconds");
            } else {
                speak!("✓", "{stats} • Created archive");
            }
        }
        Ok(status) => {
            if let Some(code) = status.code() {
                speak!("✗", "{stats} • Borg exited with code {code}");
            } else {
                speak!("✗", "{stats} • Borg terminated abnormally: {status}");
            }
        }
        Err(err) => {
            speak!("✗", "{stats} • Failed to wait for Borg: {err}");
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
    fn bar_style(stats: Arc<RwLock<ArchiveStats>>) -> ProgressStyle {
        ProgressStyle::with_template("[boi] {spinner} {stats} • {wide_msg}")
            .expect("hardcoded ProgressStyle template should be valid")
            .with_key("stats", move |_: &ProgressState, w: &mut dyn fmt::Write| {
                let _ = write!(w, "{}", stats.read().unwrap());
            })
    }
}

impl Display for ArchiveStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{nfiles} N {orig} S {comp} C {ddup} D",
            nfiles = self.nfiles,
            orig = HumanBytes(self.original_size),
            comp = HumanBytes(self.compressed_size),
            ddup = HumanBytes(self.deduplicated_size),
        )
    }
}
